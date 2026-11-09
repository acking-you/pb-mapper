//! LAN first, then direct, then the relay: the default of every entry point,
//! under the features an integrator of this crate builds with.

#[test]
fn every_entry_point_defaults_to_lan_then_direct_then_relay() {
    let connect = pb_mapper::ConnectOptions::default();
    assert_eq!(connect.path, pb_mapper::PathPolicy::Auto);
    assert_eq!(connect.port_mapping, pb_mapper::PortMapping::Auto);
    assert!(connect.lan_first);
    assert_eq!(connect.lan_transport, pb_mapper::LanTransport::Quic);
    let register = pb_mapper::RegisterOptions::default();
    assert!(register.allow_direct && register.lan_first && register.lan_tcp);
    #[cfg(feature = "desktop")]
    {
        let host = pb_mapper::desktop::HostOptions::new("desktop:defaults");
        assert!(host.allow_direct && host.lan_first && host.lan_tcp && !host.allow_control);
    }
}
