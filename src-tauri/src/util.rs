pub fn generate_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    let (a, b) = bytes.split_at(8);
    format!(
        "{:016x}{:016x}",
        u64::from_le_bytes(a.try_into().unwrap()),
        u64::from_le_bytes(b.try_into().unwrap())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_id_is_32_hex_chars() {
        let id = generate_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_id_is_unique() {
        let ids: HashSet<String> = (0..100).map(|_| generate_id()).collect();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn generate_id_is_lowercase() {
        let id = generate_id();
        assert_eq!(id, id.to_lowercase());
    }
}
