use sha1::{Digest, Sha1};

pub fn bytes_to_hex_encoding(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut current, b| {
        print!("Current: {}, Byte - {}", current, b);
        current.push_str(format!("%{:02X}", b).as_str());
        println!(", New: {}", current);
        current
    })
}

pub fn sha1_hash(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    let mut hash: [u8; 20] = [0; 20];
    hasher.update(bytes);
    hash.copy_from_slice(&hasher.finalize()[..]);
    hash
}

#[cfg(test)]
mod tests {
    use crate::utils;

    #[test]
    fn bytes_to_hex_encoding_works() {
        let bytes = [0x12];
        assert_eq!(utils::bytes_to_hex_encoding(&bytes), "%12");
    }

    #[test]
    fn sha1_hash_works() {
        let val = String::from("test_123_jsdknv");
        let expected_hash: [u8; 20] = [
            0xC4, 0xD4, 0x85, 0xCA, 0xBF, 0x76, 0x52, 0xBF, 0xE2, 0x2A, 0x50, 0xED, 0xC4, 0xC0,
            0x6A, 0xAD, 0x0C, 0x2B, 0x7B, 0x2E,
        ];
        assert_eq!(utils::sha1_hash(&val.into_bytes()[..]), expected_hash);
    }
}
