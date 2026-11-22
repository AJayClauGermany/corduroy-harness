//! Host-side bindings generated from `wit/corduroy.wit`.

/// What a plugin's `credential` handle stands for. The handle is an index into the
/// instance's list of credential names, so the type itself holds nothing.
#[derive(Debug)]
#[expect(
    unreachable_pub,
    reason = "the generated bindings re-export it with `pub use`, which needs it public"
)]
pub struct Credential;

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "plugin",
    with: {
        "corduroy:plugin/types.credential": crate::plugin::bindings::Credential,
    },
});
