//! The GPU plugin in a browser: each backend function a program calls is
//! encoded onto the wire to the page's WebGPU (`crate::wire`), which the
//! GPU agent decodes and runs. Functions this module does not define raise
//! that they are not available on the web (the generated `backend`).
