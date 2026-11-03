FROM rust:1.99.1-alpine AS build

RUN apk add --no-cache musl-dev

WORKDIR /app

COPY . .

RUN cargo build --release --locked

FROM scratch

COPY --from=build /app/target/release/spa_serve /usr/local/bin/spa_serve

EXPOSE 80/tcp

CMD ["spa_serve", "/www"]
