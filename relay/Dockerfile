# Ferrite relay: opaque WS signal + TTL blob store. No UDP needed by any
# client (TURN is bound but never requested), so this runs on any plain
# HTTP/WS host. Build context is the repo root; see render.yaml.
FROM golang:1.27-alpine AS build
WORKDIR /src
COPY relay/go.mod relay/go.sum ./
RUN go mod download
COPY relay/main.go relay/blob.go ./
RUN CGO_ENABLED=0 go build -trimpath -o /ferrite-relay .

FROM alpine:3
RUN adduser -D relay
WORKDIR /home/relay
COPY --from=build /ferrite-relay /usr/local/bin/ferrite-relay
RUN chown relay:relay /home/relay
USER relay
# $PORT when the host assigns one, else 8080. FERRITE_RELAY_PUBLIC_IP
# must be set (any value; nothing uses TURN) or the process exits.
ENTRYPOINT ["/usr/local/bin/ferrite-relay"]
