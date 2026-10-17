//! Bounded, pointer-free socket broker frames shared by the host and contained component.
pub const VERSION: u32 = 1;
pub const REQUEST_SIZE: usize = 40;
pub const RESPONSE_SIZE: usize = 664;
pub const PROTOCOL_SIZE: usize = 628;
pub const SOCKET: u32 = 1;
pub const REGISTER: u32 = 2;
pub const ACK: u32 = 3;
pub const MAX_CLIENTS: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub operation: u32,
    pub id: u32,
    pub family: i32,
    pub kind: i32,
    pub protocol: i32,
    pub flags: u32,
    pub child: u64,
}
impl Request {
    pub fn encode(self) -> [u8; REQUEST_SIZE] {
        let mut bytes = [0; REQUEST_SIZE];
        for (offset, value) in [
            (0, VERSION),
            (4, self.operation),
            (8, self.id),
            (12, self.family as u32),
            (16, self.kind as u32),
            (20, self.protocol as u32),
            (24, self.flags),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[32..40].copy_from_slice(&self.child.to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8; REQUEST_SIZE]) -> Option<Self> {
        let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if word(0) != VERSION || word(8) == 0 || word(28) != 0 {
            return None;
        }
        let request = Self {
            operation: word(4),
            id: word(8),
            family: word(12) as i32,
            kind: word(16) as i32,
            protocol: word(20) as i32,
            flags: word(24),
            child: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
        };
        let empty =
            request.family == 0 && request.kind == 0 && request.protocol == 0 && request.flags == 0;
        match request.operation {
            SOCKET if request.child == 0 => Some(request),
            REGISTER if empty && request.child != 0 && request.child != u64::MAX => Some(request),
            ACK if empty && request.child == 0 => Some(request),
            _ => None,
        }
    }
    pub fn supported_socket(&self) -> bool {
        self.operation == SOCKET
            && [2, 23].contains(&self.family)
            && self.flags & !0x81 == 0
            && matches!((self.kind, self.protocol), (1, 0 | 6) | (2, 0 | 17))
            && self.child == 0
    }
}
pub fn response(id: u32, error: u32, request: isize, reply: isize) -> [u8; RESPONSE_SIZE] {
    let mut bytes = [0; RESPONSE_SIZE];
    bytes[..4].copy_from_slice(&VERSION.to_le_bytes());
    bytes[4..8].copy_from_slice(&id.to_le_bytes());
    bytes[8..12].copy_from_slice(&error.to_le_bytes());
    bytes[16..24].copy_from_slice(&(request as u64).to_le_bytes());
    bytes[24..32].copy_from_slice(&(reply as u64).to_le_bytes());
    bytes
}
pub fn response_error(bytes: &[u8; RESPONSE_SIZE], id: u32) -> Result<u32, u32> {
    if bytes[..4] != VERSION.to_le_bytes()
        || bytes[4..8] != id.to_le_bytes()
        || bytes[12..16] != [0; 4]
        || bytes[660..] != [0; 4]
    {
        return Err(10022);
    }
    Ok(u32::from_le_bytes(bytes[8..12].try_into().unwrap()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unrelated_socket_authority_and_malformed_frames() {
        let request = Request {
            operation: SOCKET,
            id: 1,
            family: 2,
            kind: 1,
            protocol: 6,
            flags: 0x81,
            child: 0,
        };
        assert!(
            Request::decode(&request.encode())
                .unwrap()
                .supported_socket()
        );
        for (family, kind, protocol, flags) in [
            (1, 1, 6, 1),
            (2, 3, 0, 1),
            (2, 1, 17, 1),
            (2, 2, 6, 1),
            (2, 1, 6, 0x40),
        ] {
            assert!(
                !Request {
                    family,
                    kind,
                    protocol,
                    flags,
                    ..request
                }
                .supported_socket()
            );
        }
        for at in [0, 28, 32] {
            let mut bytes = request.encode();
            bytes[at] = 99;
            assert!(Request::decode(&bytes).is_none());
        }
        let mut reply = response(1, 0, 0, 0);
        assert_eq!(response_error(&reply, 2), Err(10022));
        reply[660] = 1;
        assert_eq!(response_error(&reply, 1), Err(10022));
    }
}
