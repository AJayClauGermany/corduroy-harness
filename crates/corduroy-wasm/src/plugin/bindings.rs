//! Host-side bindings generated from `wit/corduroy.wit`.

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "plugin",
});
