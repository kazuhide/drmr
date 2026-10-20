//! Preserve S3's read/write idle limits across the HTTP client's transport boundary.

use std::time::{Duration, Instant};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::{
    Duration as TransportDuration, Instant as TransportInstant,
};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, NextTimeout, RustlsConnector, TcpConnector, Transport,
};

pub(super) fn agent(idle: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(idle))
        .max_redirects(0)
        .http_status_as_error(false)
        // Signed requests must not start using environment proxies after an HTTP client update.
        .proxy(None)
        .build();
    with_config(config, idle)
}

fn with_config(config: ureq::config::Config, idle: Duration) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        ().chain(TcpConnector::default())
            .chain(IdleConnector(idle))
            .chain(RustlsConnector::default()),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct IdleConnector(Duration);

impl<T: Transport> Connector<T> for IdleConnector {
    type Out = IdleTransport<T>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| IdleTransport {
            inner,
            idle: self.0,
            // Retain the start of TCP setup, including time spent before this connector.
            connect_deadline: connection_deadline(details.now, details.timeout.after),
        }))
    }
}

#[derive(Debug)]
struct IdleTransport<T> {
    inner: T,
    idle: Duration,
    connect_deadline: Option<Instant>,
}

impl<T> IdleTransport<T> {
    fn timeout(
        &self,
        timeout: NextTimeout,
        reason: ureq::Timeout,
    ) -> Result<NextTimeout, ureq::Error> {
        // Later request/response phases must not reuse an expired handshake deadline.
        let timeout = if timeout.reason == ureq::Timeout::Connect {
            let remaining = self
                .connect_deadline
                .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ureq::Error::Timeout(ureq::Timeout::Connect))?;
            bound(timeout, remaining, ureq::Timeout::Connect)
        } else {
            timeout
        };
        Ok(bound(timeout, self.idle, reason))
    }
}

fn connection_deadline(start: TransportInstant, budget: TransportDuration) -> Option<Instant> {
    match (start, budget) {
        (TransportInstant::Exact(start), TransportDuration::Exact(budget)) => {
            start.checked_add(budget)
        }
        _ => None,
    }
}

impl<T: Transport> Transport for IdleTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let timeout = self.timeout(timeout, ureq::Timeout::SendBody)?;
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let timeout = self.timeout(timeout, ureq::Timeout::RecvBody)?;
        self.inner.await_input(timeout)
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
mod tls_tests;

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
            connect_deadline: None,
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
            connect_deadline: None,
        };
        assert!(
            matches!(transport.await_input(timeout(Duration::from_secs(2))), Err(ureq::Error::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
        );
        assert_eq!(transport.inner.reads.len(), 1);
        assert_eq!(transport.inner.writes.len(), 0);
    }

    #[test]
    fn an_expired_connect_budget_cannot_be_restarted_by_another_tls_read() {
        let before_tcp = Instant::now()
            .checked_sub(Duration::from_millis(150))
            .unwrap();
        let connect_deadline = connection_deadline(
            TransportInstant::Exact(before_tcp),
            Duration::from_millis(100).into(),
        );
        assert_eq!(
            connect_deadline,
            before_tcp.checked_add(Duration::from_millis(100))
        );
        let mut transport = IdleTransport {
            inner: Probe::default(),
            idle: Duration::from_millis(300),
            connect_deadline,
        };
        let deadline = NextTimeout {
            after: Duration::from_secs(2).into(),
            reason: ureq::Timeout::Connect,
        };
        assert!(matches!(
            transport.await_input(deadline),
            Err(ureq::Error::Timeout(ureq::Timeout::Connect))
        ));
        assert_eq!(transport.inner.reads.len(), 0);
        assert!(
            !transport
                .await_input(timeout(Duration::from_secs(2)))
                .unwrap()
        );
        assert_eq!(transport.inner.reads.len(), 1);
    }
}
