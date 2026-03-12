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
