/// Version of the Elektron Net node this pool release is built and tested against.
pub const NODE: &str = "4.0.6";
/// Version of the pool itself (Cargo.toml), for example 0.2.0 or 0.2.0-rc1.
pub const POOL: &str = env!("CARGO_PKG_VERSION");

/// Release name shown in the web footer: v<node>-p<pool>, for example v2.0.6-p0.2.0-rc1.
pub fn release() -> String {
    format!("v{NODE}-p{POOL}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_name_has_node_and_pool_part() {
        let r = super::release();
        assert!(r.starts_with("v") && r.contains("-p"), "{r}");
        assert!(r.ends_with(super::POOL));
    }
}
