FROM rust:1
WORKDIR /app

RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo,from=rust,source=/usr/local/cargo \
    cargo install mzoon --git https://github.com/MoonZoon/MoonZoon --rev 8ee7546bf42ff8e87d14d498bf9863728a18ebe5 --locked

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo,from=rust,source=/usr/local/cargo \
    --mount=type=cache,target=target \
    /usr/local/cargo/bin/mzoon build -r

RUN --mount=type=cache,target=target \
    ["cp", "./target/release/backend", "/usr/local/bin/moon_app"]

ENTRYPOINT ["moon_app"]
