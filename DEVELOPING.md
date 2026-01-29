# Developing ddp-router

## Building on Windows (msvc)

Ensure you have rust `v1.79.0` installed. Attempting to build with a later version
of the toolchain may result in build errors due to changes in dependencies. For
example, using `v1.86.0-nightly` would produce an error like the following:

```shell
# > error[E0282]: type annotations needed for `Box<_>`
# >   --> C:/Users/xxx/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/time-0.3.31/src/format_description/parse/mod.rs:83:9
# >    |
# > 83 |     let items = format_items
# >    |         ^^^^^
# > ...
# > 86 |     Ok(items.into())
# >    |              ---- type must be known at this point
# >    |
# >    = note: this is an inference error on crate `time` caused by an API change in Rust 1.80.0; update `time` to version `>=0.3.35` by calling `cargo update`
# > 
#
# some lines omitted for brevity
# 
# > 
# > For more information about this error, try `rustc --explain E0282`.
# > error: could not compile `time` (lib) due to 1 previous error
# > warning: build failed, waiting for other jobs to finish...
```

You can manage Rust versions using `rustup`. As of 1/28/2026 there doesn't seem
to be a build of the full profile for `1.79.0-x86_64-pc-windows-msvc` 

```shell
rustup install 1.79.0
# > warning: downloading with complete profile isn't recommended unless you are a developer of the rust language
# > info: syncing channel updates for '1.79.0-x86_64-pc-windows-msvc'
# > info: latest update on 2024-06-13, rust version 1.79.0 (129f3b996 2024-06-10)
# > error: some components unavailable for download for channel '1.79.0': 'miri' for target 'x86_64-pc-windows-msvc', 'rustc-codegen-cranelift' for target 'x86_64-pc-windows-msvc'If you don't need the components, you could try a minimal installation with:
# > 
# >     rustup toolchain add 1.79.0 --profile minimal
# > 
# > If you require these components, please install and use the latest successful build version,
# > which you can find at <https://rust-lang.github.io/rustup-components-history>.
# > 
# > After determining the correct date, install it with a command such as:
# > 
# >     rustup toolchain install nightly-2018-12-27
# > 
# > Then you can use the toolchain with commands such as:
# > 
# >     cargo +nightly-2018-12-27 build
```

Luckily we can install a minimal profile and still build the project successfully:

```shell
rustup toolchain add 1.79.0 --profile minimal
# > info: syncing channel updates for '1.79.0-x86_64-pc-windows-msvc'
# > info: latest update on 2024-06-13, rust version 1.79.0 (129f3b996 2024-06-10)
# > info: downloading component 'cargo'
# > info: downloading component 'rust-std'
# >  18.3 MiB /  18.3 MiB (100 %)  12.3 MiB/s in  1s ETA:  0s
# > info: downloading component 'rustc'
# >  57.7 MiB /  57.7 MiB (100 %)   9.6 MiB/s in  6s ETA:  0s
# > info: installing component 'cargo'
# > info: installing component 'rust-std'
# > info: installing component 'rustc'
# >  57.7 MiB /  57.7 MiB (100 %)  25.1 MiB/s in  2s ETA:  0s
# > 
# >   1.79.0-x86_64-pc-windows-msvc installed - rustc 1.79.0 (129f3b996 2024-06-10)
# > 
# > info: checking for self-update
# > info: downloading self-update
```

If you have other versions of Rust installed, you can set an override for the
project directory like so:

```shell
rustup override set 1.79.0
# > info: override toolchain for 'C:/path/to/ddp-router' set to '1.79.0-x86_64-pc-windows-msvc'
```

Finally, build the project with debug symbols like so:

```shell
cargo build
# ... output omitted for brevity ...
# > Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.59s
```

Or in production mode:

```shell
cargo build --release
# ... output omitted for brevity ...
# > Finished `release` profile [optimized] target(s) in 52.68s
```
