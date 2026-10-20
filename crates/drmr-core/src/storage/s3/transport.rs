//! Preserve S3's read/write idle limits across the HTTP client's transport boundary.

use std::time::Duration;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

pub(super) fn agent(idle: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(idle))
        .max_redirects(0)
        .http_status_as_error(false)
        // Signed requests must not start using environment proxies after an HTTP client update.
        .proxy(None)
        .build();
    ureq::Agent::with_parts(
        config,
        DefaultConnector::default().chain(IdleConnector(idle)),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct IdleConnector(Duration);

impl<T: Transport> Connector<T> for IdleConnector {
    type Out = IdleTransport<T>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| IdleTransport {
            inner,
            idle: self.0,
        }))
    }
}

#[derive(Debug)]
struct IdleTransport<T> {
    inner: T,
    idle: Duration,
}

impl<T: Transport> Transport for IdleTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner
            .transmit_output(amount, bound(timeout, self.idle, ureq::Timeout::SendBody))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.inner
            .await_input(bound(timeout, self.idle, ureq::Timeout::RecvBody))
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

fn bound(timeout: NextTimeout, idle: Duration, reason: ureq::Timeout) -> NextTimeout {
    if idle < *timeout.after {
        NextTimeout {
            after: idle.into(),
            reason,
        }
    } else {
        timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use ureq::unversioned::transport::LazyBuffers;

    #[derive(Debug)]
    struct Probe {
        buffers: LazyBuffers,
        reads: Vec<NextTimeout>,
        writes: Vec<(usize, NextTimeout)>,
        interrupted: bool,
    }

    impl Default for Probe {
        fn default() -> Self {
            Self {
                buffers: LazyBuffers::new(16, 16),
                reads: Vec::new(),
                writes: Vec::new(),
                interrupted: false,
            }
        }
    }

    impl Transport for Probe {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }

        fn transmit_output(
            &mut self,
            amount: usize,
            timeout: NextTimeout,
        ) -> Result<(), ureq::Error> {
            self.writes.push((amount, timeout));
            Ok(())
        }

        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
            self.reads.push(timeout);
            if self.interrupted {
                Err(ureq::Error::Io(io::Error::from(io::ErrorKind::Interrupted)))
            } else {
                Ok(false)
            }
        }

        fn is_open(&mut self) -> bool {
            true
        }

        fn is_tls(&self) -> bool {
            true
        }
    }

    fn timeout(after: Duration) -> NextTimeout {
        NextTimeout {
            after: after.into(),
            reason: ureq::Timeout::Global,
        }
    }

    #[test]
    fn read_and_write_keep_the_earlier_deadline_and_reset_only_the_idle_limit() {
        let mut transport = IdleTransport {
            inner: Probe::default(),
            idle: Duration::from_millis(300),
        };
        for budget in [Duration::from_millis(50), Duration::from_secs(2)] {
            let deadline = timeout(budget);
            assert!(!transport.await_input(deadline).unwrap());
            transport.transmit_output(17, deadline).unwrap();
        }
        assert_eq!(*transport.inner.reads[0].after, Duration::from_millis(50));
        assert_eq!(transport.inner.reads[0].reason, ureq::Timeout::Global);
        assert_eq!(*transport.inner.reads[1].after, Duration::from_millis(300));
        assert_eq!(transport.inner.reads[1].reason, ureq::Timeout::RecvBody);
        assert_eq!(transport.inner.writes[0].0, 17);
        assert_eq!(
            *transport.inner.writes[0].1.after,
            Duration::from_millis(50)
        );
        assert_eq!(transport.inner.writes[0].1.reason, ureq::Timeout::Global);
        assert_eq!(
            *transport.inner.writes[1].1.after,
            Duration::from_millis(300)
        );
        assert_eq!(transport.inner.writes[1].1.reason, ureq::Timeout::SendBody);
        assert!(transport.is_open());
        assert!(transport.is_tls());
    }

    #[test]
    fn interrupted_reads_propagate_without_replaying_a_request() {
        let mut transport = IdleTransport {
            inner: Probe {
                interrupted: true,
                ..Probe::default()
            },
            idle: Duration::from_millis(300),
        };
        assert!(
            matches!(transport.await_input(timeout(Duration::from_secs(2))), Err(ureq::Error::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
        );
        assert_eq!(transport.inner.reads.len(), 1);
        assert_eq!(transport.inner.writes.len(), 0);
    }
}
