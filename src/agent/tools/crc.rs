const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = 0xEDB88320 ^ (crc >> 1);
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Incremental CRC for bounded-memory file reads.
pub(crate) struct Crc32(u32);
impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(!0)
    }
    pub(crate) fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 = CRC32_TABLE[((self.0 as u8) ^ byte) as usize] ^ (self.0 >> 8);
        }
    }
    pub(crate) fn hex(&self) -> String {
        format!("{:08x}", !self.0)
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    !crc.0
}

pub fn crc32_hex(data: &[u8]) -> String {
    format!("{:08x}", crc32(data))
}

/// Match the read revision without allocating a normalized copy of the file.
pub(crate) fn crc32_text(content: &str) -> String {
    let mut crc = Crc32::new();
    let mut sections = content.split("\r\n").peekable();
    while let Some(section) = sections.next() {
        crc.update(section.as_bytes());
        if sections.peek().is_some() {
            crc.update(b"\n");
        }
    }
    crc.hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_crc_matches_standard_vector() {
        let mut crc = Crc32::new();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.hex(), "cbf43926");
        assert_eq!(crc32_hex(b"123456789"), crc.hex());
    }
    #[test]
    fn text_revision_matches_crlf_normalization() {
        for text in [
            "",
            "one\n",
            "one\r\ntwo\nlast",
            "\r\r\n",
            "é🦀\r\n",
            "last\r",
        ] {
            assert_eq!(
                crc32_text(text),
                crc32_hex(text.replace("\r\n", "\n").as_bytes())
            );
        }
    }
}
