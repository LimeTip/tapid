//! Fixed local guard messages. Handles refer only to the authenticated caller's table.
pub const SIZE: usize = 48;
pub const VERSION: u32 = 1;
pub const REGISTER: u32 = 1;
pub const FINISH: u32 = 2;
pub const STATUS: u32 = 3;
pub const ENFORCED: u32 = 3;
const MAGIC: &[u8; 8] = b"TPDGARD1";
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub operation: u32,
    pub process: u64,
    pub job: u64,
}
impl Request {
    pub fn encode(self) -> [u8; SIZE] {
        let mut b = [0; SIZE];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&VERSION.to_le_bytes());
        b[12..16].copy_from_slice(&self.operation.to_le_bytes());
        b[16..24].copy_from_slice(&self.process.to_le_bytes());
        b[24..32].copy_from_slice(&self.job.to_le_bytes());
        b
    }
    pub fn decode(b: &[u8; SIZE]) -> Option<Self> {
        if &b[..8] != MAGIC || b[8..12] != VERSION.to_le_bytes() || b[32..].iter().any(|v| *v != 0)
        {
            return None;
        }
        let r = Self {
            operation: u32::from_le_bytes(b[12..16].try_into().ok()?),
            process: u64::from_le_bytes(b[16..24].try_into().ok()?),
            job: u64::from_le_bytes(b[24..32].try_into().ok()?),
        };
        match r.operation {
            REGISTER if [r.process, r.job].iter().all(|v| *v != 0 && *v != u64::MAX) => Some(r),
            FINISH | STATUS if r.process == 0 && r.job == 0 => Some(r),
            _ => None,
        }
    }
}
pub fn response(error: u32, flags: u32, active: u32, retained: u32) -> [u8; SIZE] {
    let mut b = [0; SIZE];
    b[..8].copy_from_slice(MAGIC);
    for (at, value) in [
        (8, VERSION),
        (12, error),
        (16, flags),
        (20, active),
        (24, retained),
    ] {
        b[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    b
}
pub fn decode_response(b: &[u8; SIZE]) -> Option<(u32, u32, u32, u32)> {
    if &b[..8] != MAGIC || b[8..12] != VERSION.to_le_bytes() || b[28..].iter().any(|v| *v != 0) {
        return None;
    }
    let w = |at| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    Some((w(12), w(16), w(20), w(24)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_frames_reject_unknown_actions_reserved_data_and_invalid_handles() {
        let r = Request {
            operation: REGISTER,
            process: 12,
            job: 16,
        };
        assert!(Request::decode(&r.encode()).is_some());
        for at in [0, 8, 12, 32, 47] {
            let mut b = r.encode();
            b[at] = 255;
            assert!(Request::decode(&b).is_none());
        }
        for handle in [0, u64::MAX] {
            assert!(
                Request::decode(
                    &Request {
                        process: handle,
                        ..r
                    }
                    .encode()
                )
                .is_none()
            );
        }
        assert!(
            Request::decode(
                &Request {
                    operation: FINISH,
                    ..r
                }
                .encode()
            )
            .is_none()
        );
        let mut b = response(0, ENFORCED, 1, 0);
        assert_eq!(decode_response(&b), Some((0, ENFORCED, 1, 0)));
        b[47] = 1;
        assert!(decode_response(&b).is_none());
    }
}
