#![forbid(unsafe_code)]
#![allow(clippy::all)]
pub mod isolate_host_capnp {
    include!(concat!(env!("OUT_DIR"), "/isolate_host_capnp.rs"));
}
pub mod isolate_worker_source_capnp {
    include!(concat!(env!("OUT_DIR"), "/isolate_worker_source_capnp.rs"));
}
pub mod byte_stream_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/capnp/compat/byte_stream_capnp.rs"
    ));
}
pub mod http_over_capnp_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/capnp/compat/http_over_capnp_capnp.rs"
    ));
}
pub mod grain_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/grain_capnp.rs"));
}
pub mod web_session_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/web_session_capnp.rs"));
}
pub mod util_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/util_capnp.rs"));
}
pub mod identity_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/identity_capnp.rs"));
}
pub mod activity_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/activity_capnp.rs"));
}
pub mod powerbox_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/powerbox_capnp.rs"));
}

pub mod supervisor_probe_capnp {
    include!(concat!(env!("OUT_DIR"), "/supervisor_probe_capnp.rs"));
}
pub mod api_session_capnp {
    include!(concat!(env!("OUT_DIR"), "/sandstorm/api_session_capnp.rs"));
}

pub mod isolate_bridge_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/sandstorm/isolate_bridge_capnp.rs"
    ));
}

pub mod outbound_http_session_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/sandstorm/outbound_http_session_capnp.rs"
    ));
}

pub mod route_session_capnp {
    include!(concat!(env!("OUT_DIR"), "/route_session_capnp.rs"));
}
