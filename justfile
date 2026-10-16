prefix := env_var('HOME') + "/.local/bin"

# Build target/release/dt. Quit a running dt before you start this binary.
build:
    cargo build --release

# Build the release binary and open the client.
run: build
    ./target/release/dt

# Install the release binary as ~/.local/bin/dt.
install: build
    mkdir -p "{{prefix}}"
    install -m 755 target/release/dt "{{prefix}}/dt"

# Remove ~/.local/bin/dt.
uninstall:
    rm -f "{{prefix}}/dt"
