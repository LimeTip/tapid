//! Pointer-free native hostname lookup messages on the authenticated network channel.
use super::network_protocol as wire;
pub const MAX_ADDRESSES: usize = 32;
const CANONICAL: usize = 32 + MAX_ADDRESSES * 48;
pub const ALLOWED_FLAGS: u32 = 1 | 2 | 4 | 8 | 256 | 1024 | 2048;
#[derive(Debug)]
pub struct Query {
    pub name: Vec<u16>,
    pub service: Vec<u16>,
    pub family: i32,
    pub kind: i32,
    pub protocol: i32,
    pub flags: u32,
}
fn string(bytes: &[u8]) -> Option<Vec<u16>> {
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let end = units.iter().position(|u| *u == 0)?;
    if units[end..].iter().any(|u| *u != 0) || String::from_utf16(&units[..end]).is_err() {
        return None;
    }
    Some(units[..=end].to_vec())
}
impl Query {
    pub fn decode(bytes: &[u8; wire::REQUEST_SIZE], request: &wire::Request) -> Option<Self> {
        if request.operation != wire::LOOKUP
            || ![0, 2, 23].contains(&request.family)
            || ![0, 1, 2].contains(&request.kind)
            || ![0, 6, 17].contains(&request.protocol)
            || request.flags & !ALLOWED_FLAGS != 0
            || matches!((request.kind, request.protocol), (1, 17) | (2, 6))
        {
            return None;
        }
        let mut name = string(&bytes[40..552])?;
        let mut service = string(&bytes[552..616])?;
        if request.child & 1 == 0 {
            if name != [0] {
                return None;
            }
            name.clear();
        }
        if request.child & 2 == 0 {
            if service != [0] {
                return None;
            }
            service.clear();
        }
        if name.is_empty() && service.is_empty() {
            return None;
        }
        Some(Self {
            name,
            service,
            family: request.family,
            kind: request.kind,
            protocol: request.protocol,
            flags: request.flags,
        })
    }
    pub fn encode(&self, id: u32) -> Option<[u8; wire::REQUEST_SIZE]> {
        let header = wire::Request {
            operation: wire::LOOKUP,
            id,
            family: self.family,
            kind: self.kind,
            protocol: self.protocol,
            flags: self.flags,
            child: u64::from(!self.name.is_empty()) | (u64::from(!self.service.is_empty()) << 1),
        };
        let mut bytes = header.encode();
        if self.name.len() > 256 {
            return None;
        }
        for (unit, out) in self
            .name
            .iter()
            .zip(bytes[40..552].as_chunks_mut::<2>().0.iter_mut())
        {
            out.copy_from_slice(&unit.to_le_bytes());
        }
        if self.service.len() > 32 {
            return None;
        }
        for (unit, out) in self
            .service
            .iter()
            .zip(bytes[552..].as_chunks_mut::<2>().0.iter_mut())
        {
            out.copy_from_slice(&unit.to_le_bytes());
        }
        Self::decode(&bytes, &header)?;
        Some(bytes)
    }
}
#[derive(Debug)]
pub struct Address {
    pub flags: i32,
    pub family: i32,
    pub kind: i32,
    pub protocol: i32,
    pub bytes: Vec<u8>,
}
#[derive(Debug)]
pub struct Answer {
    pub canonical: Vec<u16>,
    pub addresses: Vec<Address>,
}
impl Answer {
    #[allow(dead_code)] // Host-only DNS response construction in this shared wire module.
    pub fn encode(&self, id: u32) -> Option<[u8; wire::RESPONSE_SIZE]> {
        if self.addresses.is_empty()
            || self.addresses.len() > MAX_ADDRESSES
            || self.canonical.len() > 256
        {
            return None;
        }
        let mut bytes = wire::response(id, 0, 0, 0);
        bytes[16..20].copy_from_slice(&(self.addresses.len() as u32).to_le_bytes());
        for (address, row) in self
            .addresses
            .iter()
            .zip(bytes[32..CANONICAL].as_chunks_mut::<48>().0.iter_mut())
        {
            if address.bytes.len() > 28 {
                return None;
            }
            for (value, out) in [
                address.flags as u32,
                address.family as u32,
                address.kind as u32,
                address.protocol as u32,
                address.bytes.len() as u32,
            ]
            .iter()
            .zip(row[..20].as_chunks_mut::<4>().0.iter_mut())
            {
                out.copy_from_slice(&value.to_le_bytes());
            }
            row[20..20 + address.bytes.len()].copy_from_slice(&address.bytes);
        }
        for (unit, out) in self.canonical.iter().zip(
            bytes[CANONICAL..CANONICAL + 512]
                .as_chunks_mut::<2>()
                .0
                .iter_mut(),
        ) {
            out.copy_from_slice(&unit.to_le_bytes());
        }
        Self::decode(&bytes)?;
        Some(bytes)
    }
    pub fn decode(bytes: &[u8; wire::RESPONSE_SIZE]) -> Option<Self> {
        let count = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
        if count == 0 || count > MAX_ADDRESSES || bytes[20..32].iter().any(|b| *b != 0) {
            return None;
        }
        let canonical = string(&bytes[CANONICAL..CANONICAL + 512])?;
        let mut addresses = Vec::with_capacity(count);
        for row in bytes[32..32 + count * 48].as_chunks::<48>().0.iter() {
            let word = |at| u32::from_le_bytes(row[at..at + 4].try_into().unwrap());
            let family = word(4) as i32;
            let length = word(16) as usize;
            if !matches!((family, length), (2, 16) | (23, 28))
                || word(0) & !ALLOWED_FLAGS != 0
                || ![0, 1, 2].contains(&(word(8) as i32))
                || ![0, 6, 17].contains(&(word(12) as i32))
                || u16::from_le_bytes([row[20], row[21]]) != family as u16
                || row[20 + length..].iter().any(|b| *b != 0)
            {
                return None;
            }
            addresses.push(Address {
                flags: word(0) as i32,
                family,
                kind: word(8) as i32,
                protocol: word(12) as i32,
                bytes: row[20..20 + length].to_vec(),
            });
        }
        if bytes[32 + count * 48..CANONICAL].iter().any(|b| *b != 0) {
            return None;
        }
        Some(Self {
            canonical,
            addresses,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookup_frame_preserves_null_and_empty_native_inputs() {
        for name in [Vec::new(), vec![0], "localhost\0".encode_utf16().collect()] {
            let query = Query {
                name,
                service: "80\0".encode_utf16().collect(),
                family: 0,
                kind: 1,
                protocol: 6,
                flags: 0,
            };
            let frame = query.encode(1).unwrap();
            let decoded = Query::decode(&frame, &wire::Request::decode(&frame).unwrap()).unwrap();
            assert_eq!(decoded.name, query.name);
            assert_eq!(decoded.service, query.service);
        }
    }
    #[test]
    fn lookup_frames_reject_unbounded_strings_families_flags_and_addresses() {
        let mut query = Query {
            name: "localhost\0".encode_utf16().collect(),
            service: vec![0],
            family: 0,
            kind: 1,
            protocol: 0,
            flags: 0,
        };
        let frame = query.encode(1).unwrap();
        assert!(Query::decode(&frame, &wire::Request::decode(&frame).unwrap()).is_some());
        for (family, flags) in [(1, 0), (0, 0x80000000)] {
            query.family = family;
            query.flags = flags;
            assert!(query.encode(1).is_none());
        }
        query.family = 0;
        query.flags = 0;
        let valid = query.encode(1).unwrap();
        for at in [0, 28, 32, 60, 614] {
            let mut malformed = valid;
            malformed[at] = 255;
            let decoded = wire::Request::decode(&malformed)
                .and_then(|header| Query::decode(&malformed, &header));
            assert!(decoded.is_none(), "accepted malformed lookup at {at}");
        }
        query.name = vec![65; 256];
        assert!(query.encode(1).is_none());
        query.name = vec![65; 257];
        assert!(query.encode(1).is_none());
        let answer = Answer {
            canonical: vec![0],
            addresses: vec![Address {
                flags: 0,
                family: 2,
                kind: 1,
                protocol: 6,
                bytes: vec![2, 0, 0, 80, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            }],
        };
        let mut bytes = answer.encode(1).unwrap();
        assert_eq!(Answer::decode(&bytes).unwrap().addresses.len(), 1);
        for (at, value) in [
            (16, 33),
            (36, 1),
            (40, 3),
            (48, 29),
            (52, 23),
            (80, 1),
            (CANONICAL, 0xff),
        ] {
            let mut malformed = bytes;
            malformed[at] = value;
            if at == CANONICAL {
                malformed[CANONICAL + 1] = 0xd8;
            }
            assert!(
                Answer::decode(&malformed).is_none(),
                "accepted malformed address at {at}"
            );
        }
        bytes[48] = 29;
        assert!(Answer::decode(&bytes).is_none());
    }
}
