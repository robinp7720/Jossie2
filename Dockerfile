# Build stage
FROM rust:latest as builder

WORKDIR /app
COPY . .
# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies (OpenSSL for HTTPS, curl for health checks)
RUN apt-get update && apt-get install -y ca-certificates libssl3 curl && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/jossie2 /usr/local/bin/jossie2

# Copy config.toml (if you want to embed a default one, otherwise mount it)
COPY config.toml .

# Expose port
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:3000/api/health || exit 1

# Run the binary
CMD ["jossie2"]
