// Package main implements the Ferrite sync relay: authed signal routing
// plus TURN allocation. Payloads forward opaque, never decrypted here.
package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/turn/v2"
	"golang.org/x/crypto/argon2"
)

// Auth payload is {"pubkey", "ts", "sig"} over
// "ferrite-relay-auth-v1|<device_id>|<ts>". First sight pins id to key.
// Must match ferrite-sync relay_auth.
const authDomain = "ferrite-relay-auth-v1"
const maxAuthSkew = 60 // seconds
const maxMessageBytes = 256*1024 + 4096

var upgrader = websocket.Upgrader{
	CheckOrigin:     checkOrigin,
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
}

// checkOrigin allows native clients (no Origin) and browser origins only
// when allowlisted via FERRITE_RELAY_ALLOWED_ORIGINS (exact match).
func checkOrigin(r *http.Request) bool {
	origin := r.Header.Get("Origin")
	if origin == "" {
		return true
	}
	allow := os.Getenv("FERRITE_RELAY_ALLOWED_ORIGINS")
	for _, o := range strings.Split(allow, ",") {
		if strings.TrimSpace(o) == origin {
			return true
		}
	}
	return false
}

// DeviceConn is one authed device session. Writes serialize on sendMu.
type DeviceConn struct {
	ID          string
	PubKey      ed25519.PublicKey
	Conn        *websocket.Conn
	LastSeen    time.Time
	MsgCount    int
	WindowStart time.Time
	mu          sync.Mutex // rate limit window
	sendMu      sync.Mutex // websocket write lock
}

func (d *DeviceConn) send(v any) error {
	d.sendMu.Lock()
	defer d.sendMu.Unlock()
	return d.Conn.WriteJSON(v)
}

// sendClose delivers a close frame. Caller must not hold sendMu.
func (d *DeviceConn) sendClose(code int, text string) error {
	d.sendMu.Lock()
	defer d.sendMu.Unlock()
	return d.Conn.WriteMessage(websocket.CloseMessage, websocket.FormatCloseMessage(code, text))
}

// Hub routes signals between authed sessions and owns the TURN server
// plus the TTL blob store. Presence is transient map state only, never
// disk; blobs are the single deliberate disk exception (see blob.go).
type Hub struct {
	mu           sync.RWMutex
	devices      map[string]*DeviceConn
	pubkeys      map[string]string
	ips          map[string]int
	turn         *turn.Server
	turnKeys     sync.Mutex
	turnKeyCache map[string]turnKeyEntry
	blobs        *BlobStore
}

// turnKeyEntry is one cached TURN credential plus its mint time. Entries
// older than turnKeyTTL are evicted on next use: without eviction a device
// that disconnects for weeks keeps a live credential slot (and memory)
// forever on a process that otherwise holds no state.
type turnKeyEntry struct {
	key []byte
	at  time.Time
}

// TURN credential lifetime. Matches no protocol value; purely local hygiene
// for the cache above (re-derivation is deterministic, so eviction is free).
const turnKeyTTL = 24 * time.Hour

// NewHub returns a Hub with its blob store in dir (created, 0700).
// Operators point dir at persistent disk; tests pass a temp dir.
func NewHub(dir string) (*Hub, error) {
	blobs, err := NewBlobStore(dir)
	if err != nil {
		return nil, err
	}
	return &Hub{
		devices:      make(map[string]*DeviceConn),
		pubkeys:      make(map[string]string),
		ips:          make(map[string]int),
		turnKeyCache: make(map[string]turnKeyEntry),
		blobs:        blobs,
	}, nil
}

// SignalMessage is the envelope for auth, signals, pairing, TURN, and
// stored blobs. Payload stays opaque to the relay, routed by To only.
// Blob is server-set on drain (the slot id the recipient acks); clients
// must not set it on send (it is ignored there).
type SignalMessage struct {
	Type      string          `json:"type"` // "auth", "signal", "pair_init", "pair_exchange", "stun", "turn_alloc", "store", "ack"
	From      string          `json:"from"`
	To        string          `json:"to"`
	Payload   json.RawMessage `json:"payload"`
	Timestamp int64           `json:"ts"`
	Blob      string          `json:"blob,omitempty"`
}

type authPayload struct {
	PubKey string `json:"pubkey"`
	Ts     int64  `json:"ts"`
	Sig    string `json:"sig"`
}

// PairInit carries the X25519 ephemeral key and device name for pairing.
type PairInit struct {
	EphemeralPubKey []byte `json:"ephemeral_pubkey"` // X25519 32B
	DeviceName      string `json:"device_name"`
}

// verifyAuth checks signature and freshness. It binds the session to the
// claimed id so spoofed From is rejected. It does not establish trust.
func verifyAuth(deviceID string, p authPayload) (ed25519.PublicKey, bool) {
	if deviceID == "" {
		return nil, false
	}
	pubRaw, err := base64.StdEncoding.DecodeString(p.PubKey)
	if err != nil || len(pubRaw) != ed25519.PublicKeySize {
		return nil, false
	}
	sigRaw, err := base64.StdEncoding.DecodeString(p.Sig)
	if err != nil || len(sigRaw) != ed25519.SignatureSize {
		return nil, false
	}
	now := time.Now().Unix()
	if p.Ts < now-maxAuthSkew || p.Ts > now+maxAuthSkew {
		return nil, false
	}
	msg := authDomain + "|" + deviceID + "|" + strconv.FormatInt(p.Ts, 10)
	if !ed25519.Verify(ed25519.PublicKey(pubRaw), []byte(msg), sigRaw) {
		return nil, false
	}
	return ed25519.PublicKey(pubRaw), true
}

func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

func (h *Hub) handleWS(w http.ResponseWriter, r *http.Request) {
	// Cap of 20 connections per IP.
	ip, _, _ := net.SplitHostPort(r.RemoteAddr)
	h.mu.Lock()
	if h.ips[ip] >= 20 {
		h.mu.Unlock()
		http.Error(w, "too many connections", http.StatusTooManyRequests)
		return
	}
	h.ips[ip]++
	h.mu.Unlock()
	defer func() {
		h.mu.Lock()
		h.ips[ip]--
		if h.ips[ip] <= 0 {
			delete(h.ips, ip)
		}
		h.mu.Unlock()
	}()

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		// Deliberately unlogged detail: stdlib upgrade errors carry the
		// peer address, and relay logs must stay free of IPs (see
		// docs/LIMITATIONS.md). Upgrade failures are config issues
		// (origins, paths), diagnosable without the peer.
		return
	}
	defer conn.Close()

	// First message must be a signed auth claim.
	var authMsg SignalMessage
	conn.SetReadDeadline(time.Now().Add(10 * time.Second))
	conn.SetReadLimit(maxMessageBytes)
	if err := conn.ReadJSON(&authMsg); err != nil {
		return
	}
	if authMsg.Type != "auth" {
		conn.WriteJSON(map[string]string{"error": "auth required"})
		return
	}
	deviceID := authMsg.From
	if deviceID == "" {
		conn.WriteJSON(map[string]string{"error": "auth missing device id"})
		return
	}
	var ap authPayload
	if err := json.Unmarshal(authMsg.Payload, &ap); err != nil {
		conn.WriteJSON(map[string]string{"error": "auth malformed"})
		return
	}
	pubkey, ok := verifyAuth(deviceID, ap)
	if !ok {
		conn.WriteJSON(map[string]string{"error": "auth signature invalid"})
		return
	}

	h.mu.Lock()
	if pinned, seen := h.pubkeys[deviceID]; seen && pinned != ap.PubKey {
		// Reject takeovers: the slot stays with the first key.
		h.mu.Unlock()
		conn.WriteJSON(map[string]string{"error": "device id already bound to another key"})
		return
	}
	if old, taken := h.devices[deviceID]; taken {
		old.sendClose(websocket.CloseNormalClosure, "replaced")
		old.Conn.Close()
	}
	h.pubkeys[deviceID] = ap.PubKey
	dc := &DeviceConn{ID: deviceID, PubKey: pubkey, Conn: conn, LastSeen: time.Now(), WindowStart: time.Now()}
	h.devices[deviceID] = dc
	h.mu.Unlock()
	// Confirm explicitly so clients never infer acceptance from timeouts.
	dc.send(map[string]string{"status": "authed"})
	log.Printf("device %.8s connected", shortID(deviceID))
	// Drain stored blobs first (oldest-first): mail that arrived while
	// this device was away. Each carries its slot id for acking; a send
	// failure stops the drain (the client reconnects and re-drains;
	// at-least-once delivery with idempotent apply, never silent loss).
	for _, slot := range h.blobs.Pending(deviceID) {
		out := SignalMessage{
			Type:      "signal",
			From:      slot.Blob.From,
			To:        deviceID,
			Payload:   slot.Blob.Payload,
			Timestamp: slot.Blob.Ts,
			Blob:      slot.ID,
		}
		if err := dc.send(out); err != nil {
			break
		}
	}
	defer func() {
		h.mu.Lock()
		// Delete only if still the holder, never a replacement.
		if cur, ok := h.devices[deviceID]; ok && cur == dc {
			delete(h.devices, deviceID)
		}
		h.mu.Unlock()
		log.Printf("device %.8s disconnected", shortID(deviceID))
	}()

	// Presence: no disk, transient map only (5.1 stateless)
	for {
		var msg SignalMessage
		conn.SetReadDeadline(time.Now().Add(60 * time.Second))
		conn.SetReadLimit(maxMessageBytes)
		if err := conn.ReadJSON(&msg); err != nil {
			return
		}
		// Sender must match the session. Spoofed From is rejected.
		if msg.From != deviceID {
			dc.send(map[string]string{"error": "from mismatch"})
			continue
		}
		// 10 messages per second per device.
		dc.mu.Lock()
		now := time.Now()
		if now.Sub(dc.WindowStart) > time.Second {
			dc.WindowStart = now
			dc.MsgCount = 0
		}
		dc.MsgCount++
		over := dc.MsgCount > 10
		dc.mu.Unlock()
		if over {
			dc.send(map[string]string{"error": "rate limited"})
			continue
		}

		// Forward opaque ciphertext, routed by To only.
		switch msg.Type {
		case "erase_me":
			// GDPR self-erasure: drop the session, the id→key pin, cached
			// TURN credentials, AND every blob addressed to this device.
			// Blobs this device stored for others are ciphertext for
			// others (opaque here by construction) and expire on their
			// own clock; erasure cannot reach into another device's
			// mailbox, so TTL is the backstop and the docs say so.
			h.mu.Lock()
			if cur, ok := h.devices[deviceID]; ok && cur == dc {
				delete(h.devices, deviceID)
			}
			delete(h.pubkeys, deviceID)
			h.mu.Unlock()
			h.turnKeys.Lock()
			delete(h.turnKeyCache, deviceID)
			h.turnKeys.Unlock()
			forgotten := h.blobs.ForgetRecipient(deviceID)
			log.Printf("device %.8s forgotten (%d blobs dropped)", shortID(deviceID), forgotten)
			dc.send(map[string]string{"status": "forgotten"})
			return
		case "signal", "pair_init", "pair_exchange", "turn_alloc":
			if msg.To == "" {
				dc.send(map[string]string{"error": "missing recipient"})
				continue
			}
			h.mu.RLock()
			target, ok := h.devices[msg.To]
			h.mu.RUnlock()
			if !ok {
				dc.send(map[string]string{"error": "404 Peer Offline", "to": msg.To})
				continue
			}
			// Forward under the target write lock.
			target.send(msg)
		case "stun":
			// Reply with the observed public address for ICE.
			host, port, _ := net.SplitHostPort(r.RemoteAddr)
			resp := map[string]string{"public_ip": host, "public_port": port}
			b, _ := json.Marshal(resp)
			msg.Payload = b
			dc.send(msg)
		case "store":
			// Persist one opaque payload for an offline peer (TTL blob).
			// Quota refusals are explicit; the sender retries later or
			// waits for the peer; nothing is dropped silently.
			if msg.To == "" || len(msg.Payload) == 0 {
				dc.send(map[string]string{"error": "missing recipient"})
				continue
			}
			id, refusal := h.blobs.Store(deviceID, msg.To, msg.Payload, msg.Timestamp)
			if refusal != "" {
				dc.send(map[string]string{"error": refusal, "to": msg.To})
				continue
			}
			dc.send(map[string]string{"status": "stored", "to": msg.To, "blob": id})
		case "ack":
			// Delete one delivered slot. Only the recipient's own slots
			// are deletable (id + session binding); unknown ids are a
			// no-op success; redelivered acks after a crash must not
			// error, or clients would retry forever.
			var body struct {
				Blob string `json:"blob"`
			}
			if err := json.Unmarshal(msg.Payload, &body); err != nil || body.Blob == "" {
				dc.send(map[string]string{"error": "malformed ack"})
				continue
			}
			h.blobs.Ack(deviceID, body.Blob)
			dc.send(map[string]string{"status": "acked", "blob": body.Blob})
		default:
			if msg.To != "" {
				h.mu.RLock()
				if target, ok := h.devices[msg.To]; ok {
					target.send(msg)
				}
				h.mu.RUnlock()
			}
		}
	}
}

func (h *Hub) turnAuthKey(username, realm string) ([]byte, bool) {
	// Only connected devices may allocate. Keys derive once and cache, so
	// Argon2 cost is paid per device, not per allocation. Stale entries
	// evict here (amortized, no sweeper goroutine for a map this small).
	h.mu.RLock()
	_, connected := h.devices[username]
	h.mu.RUnlock()
	if !connected {
		return nil, false
	}
	h.turnKeys.Lock()
	defer h.turnKeys.Unlock()
	h.evictStaleTurnKeys(time.Now())
	if entry, ok := h.turnKeyCache[username]; ok {
		return entry.key, true
	}
	key := argon2.IDKey([]byte(username), []byte(realm), 1, 64*1024, 1, 32)
	h.turnKeyCache[username] = turnKeyEntry{key: key, at: time.Now()}
	_ = rand.Reader
	return key, true
}

// evictStaleTurnKeys drops credentials older than turnKeyTTL. Caller must
// hold turnKeys. Split out for tests: re-derivation is deterministic, so
// eviction is always safe and never needs coordination.
func (h *Hub) evictStaleTurnKeys(now time.Time) {
	for user, entry := range h.turnKeyCache {
		if now.Sub(entry.at) > turnKeyTTL {
			delete(h.turnKeyCache, user)
		}
	}
}

// httpPort is the WS listen port: $PORT when a host assigns one
// (Render, Fly, Railway all do), 8080 for local runs.
func httpPort() string {
	if p := os.Getenv("PORT"); p != "" {
		return p
	}
	return "8080"
}

func startTURN(h *Hub) {
	udpListener, err := net.ListenPacket("udp4", "0.0.0.0:3478")
	if err != nil {
		log.Printf("TURN udp listen failed: %v", err)
		return
	}
	publicIP := os.Getenv("FERRITE_RELAY_PUBLIC_IP")
	relayIP := net.ParseIP("127.0.0.1")
	if publicIP != "" {
		if parsed := net.ParseIP(publicIP); parsed != nil {
			relayIP = parsed
		} else {
			log.Printf("FERRITE_RELAY_PUBLIC_IP invalid, using 127.0.0.1 (relayed candidates will be loopback)")
		}
	} else if os.Getenv("FERRITE_RELAY_ALLOW_LOOPBACK") != "1" {
		// Fail closed: relayed candidates carrying 127.0.0.1 look like a
		// working relay while connecting nothing. Production sets
		// FERRITE_RELAY_PUBLIC_IP; local dev sets FERRITE_RELAY_ALLOW_LOOPBACK=1.
		log.Fatalf("FERRITE_RELAY_PUBLIC_IP unset and FERRITE_RELAY_ALLOW_LOOPBACK!=1: refusing to advertise loopback TURN candidates")
	} else {
		log.Printf("FERRITE_RELAY_ALLOW_LOOPBACK=1: relayed candidates use 127.0.0.1 (dev only)")
	}
	server, err := turn.NewServer(turn.ServerConfig{
		Realm: "ferrite",
		AuthHandler: func(username string, realm string, srcAddr net.Addr) ([]byte, bool) {
			return h.turnAuthKey(username, realm)
		},
		PacketConnConfigs: []turn.PacketConnConfig{
			{PacketConn: udpListener, RelayAddressGenerator: &turn.RelayAddressGeneratorStatic{
				RelayAddress: relayIP,
				Address:      "0.0.0.0",
			}},
		},
	})
	if err != nil {
		log.Printf("TURN server failed: %v", err)
		return
	}
	h.turn = server
	log.Println("TURN listening on :3478")
	select {}
}

func main() {
	// Transport note: this process speaks plain WS on :8080 and UDP TURN on
	// :3478. Production runs it behind a TLS terminator (WS URLs are
	// device-configured, so point them at wss://); the relay itself never
	// sees plaintext credentials; SignalMessage payloads are opaque
	// ciphertext routed by To, and TURN passwords are per-device Argon2
	// derivations. Browser-origin WebSockets additionally need
	// FERRITE_RELAY_ALLOWED_ORIGINS; native clients send no Origin.
	// TTL blob slots persist under FERRITE_RELAY_DATA_DIR (default
	// ./ferrite-blobs): the relay's only disk state, bounded and
	// expiring (see blob.go). Everything else stays memory-only.
	dataDir := os.Getenv("FERRITE_RELAY_DATA_DIR")
	if dataDir == "" {
		dataDir = "./ferrite-blobs"
	}
	h, err := NewHub(dataDir)
	if err != nil {
		log.Fatalf("blob store: %v", err)
	}
	go startTURN(h)
	// Expiry backstop: delivery and store paths sweep their own
	// recipient first; this catches slots whose owners never return.
	go func() {
		for range time.Tick(time.Hour) {
			h.blobs.Sweep()
		}
	}()

	mux := http.NewServeMux()
	mux.HandleFunc("/sync", h.handleWS)
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("ok"))
	})

	srv := &http.Server{
		Addr:         ":" + httpPort(),
		Handler:      mux,
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 10 * time.Second,
	}

	go func() {
		log.Printf("relay listening on %s /sync, TURN :3478", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("listen: %v", err)
		}
	}()

	// Shutdown: close sessions, then the listeners.
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, syscall.SIGTERM, syscall.SIGINT)
	<-quit
	log.Println("SIGTERM, closing WebSockets")
	h.mu.RLock()
	for _, dc := range h.devices {
		dc.sendClose(websocket.CloseServiceRestart, "relay shutdown")
		dc.Conn.Close()
	}
	h.mu.RUnlock()
	srv.Close()
	if h.turn != nil {
		h.turn.Close()
	}
}
