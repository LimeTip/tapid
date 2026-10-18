//! Standard-user client; no reply is trusted until the held server matches protected SCM state.
use super::{
    failure, identity, pipe,
    protocol::{self, Request},
    raw,
};
use crate::execution::ExecutionError;
use std::os::windows::io::OwnedHandle;
use windows_sys::Win32::Foundation::ERROR_INVALID_DATA;
pub(in crate::execution) struct Guard {
    pipe: OwnedHandle,
    _server: OwnedHandle,
}
impl Guard {
    fn connect() -> Result<Self, u32> {
        let pipe = pipe::connect()?;
        let server = identity::server(raw(&pipe))?;
        Ok(Self {
            pipe,
            _server: server,
        })
    }
    fn exchange(&self, request: Request) -> Result<(u32, u32, u32), u32> {
        let frame = pipe::exchange(raw(&self.pipe), &request.encode())?;
        let (error, flags, active, retained) =
            protocol::decode_response(&frame).ok_or(ERROR_INVALID_DATA)?;
        if error != 0 {
            return Err(error);
        }
        Ok((flags, active, retained))
    }
    pub fn register(process: isize, job: isize) -> Result<Self, ExecutionError> {
        let guard = Self::connect().map_err(|e| failure("connection", e))?;
        let (flags, _, _) = guard
            .exchange(Request {
                operation: protocol::REGISTER,
                process: process as u64,
                job: job as u64,
            })
            .map_err(|e| failure("registration", e))?;
        if flags != protocol::ENFORCED {
            return Err(failure("enforcement acknowledgement", ERROR_INVALID_DATA));
        }
        Ok(guard)
    }
    pub fn finish(self) -> Result<(), ExecutionError> {
        self.exchange(Request {
            operation: protocol::FINISH,
            process: 0,
            job: 0,
        })
        .map_err(|e| failure("empty-Job filter cleanup", e))?;
        Ok(())
    }
    pub fn status() -> Result<(u32, u32), u32> {
        let guard = Self::connect()?;
        let (_, active, retained) = guard.exchange(Request {
            operation: protocol::STATUS,
            process: 0,
            job: 0,
        })?;
        Ok((active, retained))
    }
}
