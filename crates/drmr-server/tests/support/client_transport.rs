//! Continue an interrupted receive on the same fixture connection, never resend a request.
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, NextTimeout, Transport};

#[derive(Debug)]
pub struct ContinueReadConnector;
impl<T: Transport> Connector<T> for ContinueReadConnector {
    type Out = ContinueRead<T>;
    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(ContinueRead))
    }
}

#[derive(Debug)]
pub struct ContinueRead<T>(pub T);
impl<T: Transport> Transport for ContinueRead<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.0.transmit_output(amount, timeout)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let start = std::time::Instant::now();
        receive_with_elapsed(&mut self.0, timeout, || start.elapsed())
    }
    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }
    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

// The injected clock keeps deadline regressions deterministic without signals or sleeps.
pub fn receive_with_elapsed<T: Transport>(
    inner: &mut T,
    timeout: NextTimeout,
    mut elapsed: impl FnMut() -> std::time::Duration,
) -> Result<bool, ureq::Error> {
    loop {
        let after = if timeout.after.is_not_happening() {
            timeout.after
        } else {
            let remaining = (*timeout.after)
                .checked_sub(elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ureq::Error::Timeout(timeout.reason))?;
            remaining.into()
        };
        match inner.await_input(NextTimeout {
            after,
            reason: timeout.reason,
        }) {
            Err(ureq::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}
