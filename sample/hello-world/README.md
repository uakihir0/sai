# Hello World

A minimal WASI command that writes `Hello, world!` to standard output. It uses
`process.Env` to obtain stdout and the core `Writer` trait to write the greeting.

The project targets `wasm32-wasi`. Its `main` entry point and standard-stream
support are specified, but the current compiler does not yet implement every
stage needed to build and run this example.
