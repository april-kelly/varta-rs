# Quickly verify your work
Perform a basic code check in the top-level workspace after every major
edit by running `cargo check`. Immediately fix all errors and warnings
before moving on.

# Before considering your work complete
- Verify the top-level workspace is clippy clean: `cargo clippy`
- Verify the top-level workspace builds without warnings: `cargo build`
- Format the code: `cargo fmt`
