//! Fixed-size preparation data shared by the host and its embedded native component.
pub const VERSION: u32 = 4;
pub const REOPEN_POLICY_MASK: u32 = 1 | 2 | 4 | 8 | 0x10 | 0x20 | 0x100 | 0x10000;
pub const MAX_ANCESTORS: usize = 64;
pub const MAX_PATH_UNITS: usize = 1024;
pub const METADATA_ACCESS: u32 = 0x80;

#[derive(Clone)]
#[repr(C)]
pub struct MetadataEntry {
    pub handle: isize,
    pub length: u32,
    pub allowed_reopen: u32,
    pub path: [u16; MAX_PATH_UNITS],
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_capability_frames() {
        let mut setup = Setup::empty();
        assert!(!setup.valid());
        setup.version = VERSION;
        setup.count = 1;
        setup.entries[0].handle = 123;
        setup.entries[0].length = 3;
        setup.entries[0].path[..3].copy_from_slice(&[67, 58, 92]);
        assert!(setup.valid());
        // The no-network plan has no broker handles. A grant needs two distinct valid ends.
        for (request, response, valid) in [
            (0, 0, true),
            (123, 124, true),
            (0, 124, false),
            (123, 0, false),
            (-1, 124, false),
            (123, -1, false),
            (123, 123, false),
        ] {
            let mut channels = setup.clone();
            channels.network_request = request;
            channels.network_response = response;
            assert_eq!(channels.valid(), valid);
        }
        for count in [0, MAX_ANCESTORS as u32 + 1, u32::MAX] {
            let mut invalid = setup.clone();
            invalid.count = count;
            assert!(!invalid.valid());
        }
        for length in [0, 2, MAX_PATH_UNITS as u32, u32::MAX] {
            let mut invalid = setup.clone();
            invalid.entries[0].length = length;
            assert!(!invalid.valid());
        }
        let mut invalid = setup.clone();
        invalid.entries[0].allowed_reopen = 0x40000;
        assert!(!invalid.valid());
        let mut invalid = setup.clone();
        invalid.entries[0].path[3] = 1;
        assert!(!invalid.valid());
        let mut invalid = setup.clone();
        invalid.entries[0].path[1] = 0;
        assert!(!invalid.valid());
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<MetadataEntry>(), 2064);
            assert_eq!(std::mem::size_of::<Setup>(), 32 + MAX_ANCESTORS * 2064);
        }
    }
}

#[derive(Clone)]
#[repr(C)]
pub struct Setup {
    pub version: u32,
    pub count: u32,
    pub stdin_eof: isize,
    pub network_request: isize,
    pub network_response: isize,
    pub entries: [MetadataEntry; MAX_ANCESTORS],
}

impl Setup {
    #[allow(dead_code)] // Constructed by the host; the contained component receives/clones it.
    pub fn empty() -> Self {
        // SAFETY: the wire structure consists entirely of integers and integer arrays.
        unsafe { std::mem::zeroed() }
    }

    pub fn valid(&self) -> bool {
        self.version == VERSION
            && self.stdin_eof != -1
            && ((self.network_request == 0 && self.network_response == 0)
                || (self.network_request != 0
                    && self.network_request != -1
                    && self.network_response != 0
                    && self.network_response != -1
                    && self.network_request != self.network_response))
            && self.count > 0
            && self.count as usize <= MAX_ANCESTORS
            && self.entries[..self.count as usize].iter().all(|entry| {
                let length = entry.length as usize;
                entry.handle != 0
                    && entry.allowed_reopen & !REOPEN_POLICY_MASK == 0
                    && entry.handle != -1
                    && (3..MAX_PATH_UNITS).contains(&length)
                    && entry.path[length] == 0
                    && !entry.path[..length].contains(&0)
            })
    }
}
