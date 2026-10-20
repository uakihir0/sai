# Greeting CLI

Run this command with a name argument to print a personalized greeting:

```text
greet-cli Ada
Hello, Ada!
```

The example reads arguments from `process.Env`, reports a usage line when the
name is missing, and uses an f-string to compose the greeting. It targets
`wasm32-wasi` and assumes the host supplies the program name as `argv[0]`.
Command-line and process support are not all implemented by the current
compiler yet.
