//! Bounded PE64 import inspection. Reads are supplied by either a fixture or checked
//! committed-memory copies; malformed offsets never become unchecked pointer reads.
#[derive(Debug)]
pub struct Import {
    pub name: Vec<u8>,
    pub offset: usize,
    pub address: usize,
}

pub fn imports(mut read: impl FnMut(usize, usize) -> Option<Vec<u8>>) -> Option<Vec<Import>> {
    let header = read(0, 64)?;
    if header.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(header.get(60..64)?.try_into().ok()?) as usize;
    if !(64..=4096).contains(&pe) {
        return None;
    }
    let header = read(pe, 152)?;
    if header.get(..4)? != b"PE\0\0"
        || ![0x8664, 0xaa64].contains(&u16::from_le_bytes(header[4..6].try_into().ok()?))
        || u16::from_le_bytes(header[24..26].try_into().ok()?) != 0x20b
    {
        return None;
    }
    let size = u32::from_le_bytes(header[80..84].try_into().ok()?) as usize;
    if !(4096..=512 * 1024 * 1024).contains(&size) {
        return None;
    }
    let import = u32::from_le_bytes(header[144..148].try_into().ok()?) as usize;
    let import_size = u32::from_le_bytes(header[148..152].try_into().ok()?) as usize;
    if import == 0 || import_size < 20 || import.checked_add(import_size)? > size {
        return None;
    }
    let mut result = Vec::new();
    let mut slots = 0;
    for descriptor in (import..import + import_size).step_by(20).take(1024) {
        if descriptor.checked_add(20)? > import + import_size {
            return None;
        }
        let data = read(descriptor, 20)?;
        let names = u32::from_le_bytes(data[..4].try_into().ok()?) as usize;
        let module = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
        let table = u32::from_le_bytes(data[16..20].try_into().ok()?) as usize;
        if module == 0 {
            return Some(result);
        }
        if names == 0 || table == 0 || module >= size {
            return None;
        }
        let mut terminated = false;
        for index in 0usize..4096 {
            slots += 1;
            if slots > 8192 {
                return None;
            }
            let name_offset = names.checked_add(index.checked_mul(8)?)?;
            let offset = table.checked_add(index.checked_mul(8)?)?;
            if name_offset.checked_add(8)? > size || offset.checked_add(8)? > size {
                return None;
            }
            let name = u64::from_le_bytes(read(name_offset, 8)?.try_into().ok()?) as usize;
            if name == 0 {
                terminated = true;
                break;
            }
            if name & (1usize << 63) != 0 {
                continue;
            }
            let name = name.checked_add(2)?;
            if name >= size {
                return None;
            }
            let mut symbol = Vec::new();
            let mut ended = false;
            for index in 0..256 {
                let position = name.checked_add(index)?;
                if position >= size {
                    return None;
                }
                let byte = *read(position, 1)?.first()?;
                if byte == 0 {
                    ended = true;
                    break;
                }
                symbol.push(byte);
            }
            if !ended {
                return None;
            }
            if matches!(
                symbol.as_slice(),
                b"CreateNamedPipeA"
                    | b"CreateFileA"
                    | b"CreateFileW"
                    | b"CreateProcessW"
                    | b"GetFinalPathNameByHandleW"
            ) {
                let address = u64::from_le_bytes(read(offset, 8)?.try_into().ok()?) as usize;
                if address == 0 {
                    return None;
                }
                result.push(Import {
                    name: symbol,
                    offset,
                    address,
                });
            }
        }
        if !terminated {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image() -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        bytes[144..148].copy_from_slice(&4096u32.to_le_bytes());
        bytes[208..212].copy_from_slice(&512u32.to_le_bytes());
        bytes[212..216].copy_from_slice(&40u32.to_le_bytes());
        bytes[512..516].copy_from_slice(&600u32.to_le_bytes());
        bytes[524..528].copy_from_slice(&750u32.to_le_bytes());
        bytes[528..532].copy_from_slice(&700u32.to_le_bytes());
        bytes[600..608].copy_from_slice(&800u64.to_le_bytes());
        bytes[700..708].copy_from_slice(&0x12345678u64.to_le_bytes());
        bytes[802..814].copy_from_slice(b"CreateFileW\0");
        bytes
    }
    fn parse(bytes: &[u8]) -> Option<Vec<Import>> {
        imports(|offset, length| {
            bytes
                .get(offset..offset.checked_add(length)?)
                .map(<[u8]>::to_vec)
        })
    }
    #[test]
    fn reads_exact_import_slots_and_rejects_out_of_image_references() {
        let bytes = image();
        let slots = parse(&bytes).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].offset, 700);
        assert_eq!(slots[0].address, 0x12345678);
        assert_eq!(slots[0].name, b"CreateFileW");
        let mut invalid = bytes.clone();
        invalid[600..608].copy_from_slice(&u64::MAX.wrapping_sub(1 << 63).to_le_bytes());
        assert!(parse(&invalid).is_none());
        let mut invalid = bytes.clone();
        invalid[802..1058].fill(b'a');
        assert!(parse(&invalid).is_none());
        let mut invalid = bytes.clone();
        invalid[528..532].copy_from_slice(&4093u32.to_le_bytes());
        assert!(parse(&invalid).is_none());
        for length in [0, 63, 100, 511, 607, 703, 809] {
            assert!(parse(&bytes[..length]).is_none());
        }
    }
    #[test]
    fn malformed_offsets_and_unterminated_tables_are_rejected() {
        for pe in [0, 63, 4097, u32::MAX] {
            let mut bytes = vec![0; 512];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&pe.to_le_bytes());
            assert!(
                imports(|offset, length| bytes
                    .get(offset..offset.checked_add(length)?)
                    .map(<[u8]>::to_vec))
                .is_none()
            );
        }
    }
}
