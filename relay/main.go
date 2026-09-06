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

// Auth protocol (must match ferrite-sync relay_auth):
//
//	auth payload = {"pubkey": base64(32B ed25519), "ts": unix_sec,
//	                "sig": base64(64B ed25519 over "ferrite-relay-auth-v1|<device_id>|<ts>")}
//
// The relay pins device_id -> pubkey on first sight and rejects mismatches,
// so a spoofed `from` without the victim's private key is rejected and a
// stolen device_id cannot take over the session.
const authDomain = "ferrite-relay-auth-v1"
const maxAuthSkew = 60 // seconds
const maxMessageBytes = 256*1024 + 4096

var upgrader = websocket.Upgrader{
	CheckOrigin:     checkOrigin,
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
}

// checkOrigin: native clients send no Origin and are always allowed.
// Browser origins are allowed only when allowlisted via
// FERRITE_RELAY_ALLOWED_ORIGINS (comma-separated, exact match).
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

type DeviceConn struct {
	ID          string
	PubKey      ed25519.PublicKey
	Conn        *websocket.Conn
	LastSeen    time.Time
	MsgCount    int
	WindowStart time.Time
	mu          sync.Mutex // rate-limit window
	sendMu      sync.Mutex // serializes concurrent websocket writes
}

func (d *DeviceConn) send(v any) error {
	d.sendMu.Lock()
	defer d.sendMu.Unlock()
	return d.Conn.WriteJSON(v)
}

// sendClose delivers a websocket close frame under the write mutex.
func (d *DeviceConn) sendClose(code int, text string) error {
	d.sendMu.Lock()
	defer d.sendMu.Unlock()
	return d.Conn.WriteMessage(websocket.CloseMessage, websocket.FormatCloseMessage(code, text))
}

type Hub struct {
	mu           sync.RWMutex
	devices      map[string]*DeviceConn // device_id -> conn
	pubkeys      map[string]string      // device_id -> base64 pinned pubkey
	ips          map[string]int         // ip -> connection count
	turn         *turn.Server
	turnKeys     sync.Mutex
	turnKeyCache map[string][]byte // TURN username -> derived key
}

func NewHub() *Hub {
	return &Hub{
		devices:      make(map[string]*DeviceConn),
		pubkeys:      make(map[string]string),
		ips:          make(map[string]int),
		turnKeyCache: make(map[string][]byte),
	}
}

type SignalMessage struct {
	Type      string          `json:"type"` // "auth", "signal", "pair_init", "pair_exchange", "stun", "turn_alloc"
	From      string          `json:"from"`
	To        string          `json:"to"`
	Payload   json.RawMessage `json:"payload"`
	Timestamp int64           `json:"ts"`
}

type authPayload struct {
	PubKey string `json:"pubkey"`
	Ts     int64  `json:"ts"`
	Sig    string `json:"sig"`
}

type PairInit struct {
	EphemeralPubKey []byte `json:"ephemeral_pubkey"` // X25519 32B
	DeviceName      string `json:"device_name"`
}

// verifyAuth checks self-consistency of an auth claim: the signature must
// verify against the presented key, and the timestamp must be fresh.
// It does NOT establish trust (pairing does that e2e); it binds the WS
// session to the claimed device_id so spoofed `from` is rejected.
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
	// Connection cap per IP
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
		log.Printf("upgrade: %v", err)
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
		// Pinned key mismatch: possible takeover with a different key.
		// Reject the new session; the existing holder keeps its slot.
		h.mu.Unlock()
		conn.WriteJSON(map[string]string{"error": "device id already bound to another key"})
		return
	}
	if old, taken := h.devices[deviceID]; taken {
		// Same-key reconnect: evict the stale holder before replacing.
		old.sendClose(websocket.CloseNormalClosure, "replaced")
		old.Conn.Close()
	}
	h.pubkeys[deviceID] = ap.PubKey
	dc := &DeviceConn{ID: deviceID, PubKey: pubkey, Conn: conn, LastSeen: time.Now(), WindowStart: time.Now()}
	h.devices[deviceID] = dc
	h.mu.Unlock()
	// Explicit auth confirmation (also flushes the handshake so clients
	// never have to infer acceptance from a read timeout).
	dc.send(map[string]string{"status": "authed"})
	log.Printf("device %.8s connected", shortID(deviceID))
	defer func() {
		h.mu.Lock()
		// Only delete if we are still the holder (avoid evicting a replacement).
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
		// Sender must match the authed session: spoofed `from` rejected.
		if msg.From != deviceID {
			dc.send(map[string]string{"error": "from mismatch"})
			continue
		}
		// Rate limit 10/sec per device
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

		// Blind forwarding - never decrypt payload, only route by To
		switch msg.Type {
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
			// Forward opaque ciphertext (per-conn write mutex inside send)
			target.send(msg)
		case "stun":
			// STUN: return observed public IP/port for ICE candidate
			host, port, _ := net.SplitHostPort(r.RemoteAddr)
			resp := map[string]string{"public_ip": host, "public_port": port}
			b, _ := json.Marshal(resp)
			msg.Payload = b
			dc.send(msg)
		default:
			// Generic signal forward
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
	// TURN access requires an active authed WS session: the username must
	// be a currently-connected device_id. Derived keys are cached so the
	// Argon2 cost is paid once per device, not per allocation (DoS).
	h.mu.RLock()
	_, connected := h.devices[username]
	h.mu.RUnlock()
	if !connected {
		return nil, false
	}
	h.turnKeys.Lock()
	defer h.turnKeys.Unlock()
	if k, ok := h.turnKeyCache[username]; ok {
		return k, true
	}
	key := argon2.IDKey([]byte(username), []byte(realm), 1, 64*1024, 1, 32)
	h.turnKeyCache[username] = key
	_ = rand.Reader
	return key, true
}

func startTURN(h *Hub) {
	// Pion TURN server allocation - blind forwarding of rustls DTLS over UDP (4.3)
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
	} else {
		log.Printf("FERRITE_RELAY_PUBLIC_IP unset: relayed candidates use 127.0.0.1 (set to a public IP in production)")
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
	h := NewHub()
	go startTURN(h)

	mux := http.NewServeMux()
	mux.HandleFunc("/sync", h.handleWS)
	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("ok"))
	})

	srv := &http.Server{
		Addr:         ":8080",
		Handler:      mux,
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 10 * time.Second,
	}

	go func() {
		log.Println("relay listening on :8080 /sync, TURN :3478")
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("listen: %v", err)
		}
	}()

	// Graceful shutdown SIGTERM (5.1)
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
