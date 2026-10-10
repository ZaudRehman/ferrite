package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func mustHub(t *testing.T) *Hub {
	t.Helper()
	h, err := NewHub(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return h
}

func testKey(t *testing.T) (ed25519.PublicKey, ed25519.PrivateKey) {
	t.Helper()
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return pub, priv
}

func authMsg(t *testing.T, deviceID string, priv ed25519.PrivateKey, pub ed25519.PublicKey, ts int64) SignalMessage {
	t.Helper()
	b64 := func(b []byte) string { return base64.StdEncoding.EncodeToString(b) }
	sig := ed25519.Sign(priv, []byte(authDomain+"|"+deviceID+"|"+strconv.FormatInt(ts, 10)))
	payload, _ := json.Marshal(map[string]any{
		"pubkey": b64(pub),
		"ts":     ts,
		"sig":    b64(sig),
	})
	return SignalMessage{Type: "auth", From: deviceID, Payload: payload}
}

func dialRelay(t *testing.T, h *Hub) *websocket.Conn {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(h.handleWS))
	t.Cleanup(srv.Close)
	url := "ws" + strings.TrimPrefix(srv.URL, "http")
	c, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	return c
}

func mustAuth(t *testing.T, c *websocket.Conn, deviceID string, priv ed25519.PrivateKey, pub ed25519.PublicKey) {
	t.Helper()
	if err := c.WriteJSON(authMsg(t, deviceID, priv, pub, time.Now().Unix())); err != nil {
		t.Fatal(err)
	}
	// The relay confirms auth explicitly; anything else is a rejection.
	c.SetReadDeadline(time.Now().Add(2 * time.Second))
	var reply map[string]string
	if err := c.ReadJSON(&reply); err != nil {
		t.Fatalf("auth confirm missing: %v", err)
	}
	if reply["status"] != "authed" {
		t.Fatalf("expected auth accept, got reply: %v", reply)
	}
	c.SetReadDeadline(time.Now().Add(60 * time.Second))
}

func TestVerifyAuthVectors(t *testing.T) {
	pub, priv := testKey(t)
	ts := time.Now().Unix()
	b64 := func(b []byte) string { return base64.StdEncoding.EncodeToString(b) }
	sig := ed25519.Sign(priv, []byte(authDomain+"|dev-1|"+strconv.FormatInt(ts, 10)))
	p := authPayload{PubKey: b64(pub), Ts: ts, Sig: b64(sig)}
	if _, ok := verifyAuth("dev-1", p); !ok {
		t.Fatal("valid auth rejected")
	}
	if _, ok := verifyAuth("dev-2", p); ok {
		t.Fatal("wrong device id accepted")
	}
	old := p
	old.Ts = ts - maxAuthSkew - 1
	if _, ok := verifyAuth("dev-1", old); ok {
		t.Fatal("stale auth accepted")
	}
	future := p
	future.Ts = ts + maxAuthSkew + 1
	if _, ok := verifyAuth("dev-1", future); ok {
		t.Fatal("future auth accepted")
	}
	bad := p
	bad.PubKey = "!!!"
	if _, ok := verifyAuth("dev-1", bad); ok {
		t.Fatal("malformed key accepted")
	}
	if _, ok := verifyAuth("", p); ok {
		t.Fatal("empty device accepted")
	}
}

func TestAuthRequiredFirst(t *testing.T) {
	h := mustHub(t)
	c := dialRelay(t, h)
	if err := c.WriteJSON(SignalMessage{Type: "signal", From: "x", To: "y"}); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := c.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["error"] != "auth required" {
		t.Fatalf("got %v", reply)
	}
}

func TestSpoofedFromRejected(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	pubB, privB := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	_ = pubB
	_ = privB
	if err := a.WriteJSON(SignalMessage{Type: "signal", From: "device-b", To: "device-a"}); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := a.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["error"] != "from mismatch" {
		t.Fatalf("got %v", reply)
	}
}

func TestTakeoverWithDifferentKeyRejected(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)

	// Same id, different key: rejected, first holder keeps routing.
	pubA2, privA2 := testKey(t)
	b := dialRelay(t, h)
	if err := b.WriteJSON(authMsg(t, "device-a", privA2, pubA2, time.Now().Unix())); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := b.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(reply["error"], "already bound") {
		t.Fatalf("got %v", reply)
	}

	// Original holder still routes (offline peer answers 404, session alive).
	if err := a.WriteJSON(SignalMessage{Type: "signal", From: "device-a", To: "nobody"}); err != nil {
		t.Fatal(err)
	}
	if err := a.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["error"] != "404 Peer Offline" {
		t.Fatalf("original session broken: %v", reply)
	}
}

func TestSameKeyReconnectReplaces(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)

	b := dialRelay(t, h)
	mustAuth(t, b, "device-a", privA, pubA)
	if err := b.WriteJSON(SignalMessage{Type: "signal", From: "device-a", To: "nobody"}); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := b.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["error"] != "404 Peer Offline" {
		t.Fatalf("got %v", reply)
	}
}

func TestStaleAuthTimestampRejected(t *testing.T) {
	h := mustHub(t)
	pub, priv := testKey(t)
	c := dialRelay(t, h)
	if err := c.WriteJSON(authMsg(t, "device-a", priv, pub, time.Now().Unix()-3600)); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := c.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["error"] != "auth signature invalid" {
		t.Fatalf("got %v", reply)
	}
}

func TestOversizeMessageRejected(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	big := strings.Repeat("x", maxMessageBytes)
	if err := a.WriteJSON(SignalMessage{Type: "signal", From: "device-a", To: "nobody", Payload: json.RawMessage(`"` + big + `"`)}); err != nil {
		// Write side self limits also pass.
		return
	}
	// Server closed the connection on oversize read.
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	var reply map[string]string
	_ = a.ReadJSON(&reply)
}

func TestTurnKeyCacheEvicts(t *testing.T) {
	h := mustHub(t)
	h.turnKeyCache["gone"] = turnKeyEntry{key: []byte("old"), at: time.Now().Add(-25 * time.Hour)}
	h.turnKeyCache["fresh"] = turnKeyEntry{key: []byte("new"), at: time.Now()}
	h.evictStaleTurnKeys(time.Now())
	if _, ok := h.turnKeyCache["gone"]; ok {
		t.Fatal("stale credential survived eviction")
	}
	if _, ok := h.turnKeyCache["fresh"]; !ok {
		t.Fatal("fresh credential wrongly evicted")
	}
}

func TestTurnAuthRefusesDisconnected(t *testing.T) {
	h := mustHub(t)
	if _, ok := h.turnAuthKey("ghost", "ferrite"); ok {
		t.Fatal("disconnected device must not allocate TURN credentials")
	}
}

func TestStoreDeliverAckFlow(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	pubB, privB := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)

	// B offline: store must accept and confirm with a slot id.
	payload, _ := json.Marshal(map[string]string{"cipher": "opaque-bytes"})
	store := SignalMessage{
		Type: "store", From: "device-a", To: "device-b", Payload: payload,
		Timestamp: time.Now().Unix(),
	}
	if err := a.WriteJSON(store); err != nil {
		t.Fatal(err)
	}
	var confirm map[string]string
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	if err := a.ReadJSON(&confirm); err != nil {
		t.Fatal(err)
	}
	if confirm["status"] != "stored" || confirm["blob"] == "" || confirm["to"] != "device-b" {
		t.Fatalf("store not confirmed: %v", confirm)
	}
	if slots, _ := h.blobs.Count(); slots != 1 {
		t.Fatalf("expected 1 slot, got %d", slots)
	}

	// B connects: drain delivers the original envelope + slot id.
	b := dialRelay(t, h)
	mustAuth(t, b, "device-b", privB, pubB)
	var delivered SignalMessage
	b.SetReadDeadline(time.Now().Add(2 * time.Second))
	if err := b.ReadJSON(&delivered); err != nil {
		t.Fatal(err)
	}
	if delivered.Type != "signal" || delivered.From != "device-a" || delivered.Blob != confirm["blob"] {
		t.Fatalf("bad drain: %+v", delivered)
	}
	if string(delivered.Payload) != string(payload) {
		t.Fatal("payload must round-trip byte-identical")
	}

	// Ack deletes the slot; a reconnect drains nothing further.
	ackPayload, _ := json.Marshal(map[string]string{"blob": confirm["blob"]})
	if err := b.WriteJSON(SignalMessage{Type: "ack", From: "device-b", Payload: ackPayload}); err != nil {
		t.Fatal(err)
	}
	var acked map[string]string
	if err := b.ReadJSON(&acked); err != nil {
		t.Fatal(err)
	}
	if acked["status"] != "acked" {
		t.Fatalf("ack not confirmed: %v", acked)
	}
	if slots, _ := h.blobs.Count(); slots != 0 {
		t.Fatalf("slot survived ack: %d", slots)
	}
}

func TestStoreQuotaAndValidation(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	payload, _ := json.Marshal(map[string]string{"c": "x"})
	store := func(to string) map[string]string {
		t.Helper()
		if err := a.WriteJSON(SignalMessage{Type: "store", From: "device-a", To: to, Payload: payload}); err != nil {
			t.Fatal(err)
		}
		var reply map[string]string
		a.SetReadDeadline(time.Now().Add(2 * time.Second))
		if err := a.ReadJSON(&reply); err != nil {
			t.Fatal(err)
		}
		return reply
	}
	// Per-recipient quota is enforced at the store layer (100 rapid WS
	// stores would trip the 10/s rate limit first; that limit is
	// covered by the existing rate tests, not relitigated here).
	for i := 0; i < maxBlobsPerDevice; i++ {
		if _, refusal := h.blobs.Store("device-a", "device-b", payload, time.Now().Unix()); refusal != "" {
			t.Fatalf("slot %d refused: %q", i, refusal)
		}
	}
	if _, refusal := h.blobs.Store("device-a", "device-b", payload, time.Now().Unix()); refusal != "quota exceeded" {
		t.Fatalf("101st slot should refuse, got %q", refusal)
	}
	// Other recipients unaffected by one full mailbox.
	if _, refusal := h.blobs.Store("device-a", "device-c", payload, time.Now().Unix()); refusal != "" {
		t.Fatalf("unrelated recipient refused: %q", refusal)
	}
	// Path-traversal and empty recipients rejected over the wire, never
	// stored. Spaced out: each is one message, well under rate limits.
	for _, bad := range []string{"../../etc", "a/b", "", "x." + strings.Repeat("y", 200)} {
		if got := store(bad); got["error"] == "" {
			t.Fatalf("bad recipient accepted: %q -> %v", bad, got)
		}
		time.Sleep(150 * time.Millisecond)
	}
	if slots, _ := h.blobs.Count(); slots != maxBlobsPerDevice+1 {
		t.Fatalf("bad stores must not consume slots: %d", slots)
	}
}

func TestBlobExpiryAndRestart(t *testing.T) {
	dir := t.TempDir()
	h, err := NewHub(dir)
	if err != nil {
		t.Fatal(err)
	}
	payload := json.RawMessage(`{"c":"x"}`)
	id, refusal := h.blobs.Store("a", "b", payload, time.Now().Unix())
	if refusal != "" || id == "" {
		t.Fatalf("store refused: %q", refusal)
	}
	// Backdate the slot file past TTL, sweep, gone.
	name := "b." + id + ".blob"
	raw, _ := os.ReadFile(filepath.Join(dir, name))
	var blob storedBlob
	if err := json.Unmarshal(raw, &blob); err != nil {
		t.Fatal(err)
	}
	blob.Expires = time.Now().Add(-time.Hour).Unix()
	raw, _ = json.Marshal(blob)
	if err := os.WriteFile(filepath.Join(dir, name), raw, 0o600); err != nil {
		t.Fatal(err)
	}
	h.blobs.Sweep()
	if slots, _ := h.blobs.Count(); slots != 0 {
		t.Fatalf("expired slot survived sweep: %d", slots)
	}
	// A live slot survives a restart (same dir, new Hub).
	id2, refusal := h.blobs.Store("a", "b", payload, time.Now().Unix())
	if refusal != "" {
		t.Fatalf("store refused: %q", refusal)
	}
	h2, err := NewHub(dir)
	if err != nil {
		t.Fatal(err)
	}
	got := h2.blobs.Pending("b")
	if len(got) != 1 || got[0].ID != id2 {
		t.Fatalf("restart lost the slot: %+v", got)
	}
}

func TestEraseMeForgetsBlobs(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	pubB, privB := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	payload, _ := json.Marshal(map[string]string{"c": "x"})
	if err := a.WriteJSON(SignalMessage{Type: "store", From: "device-a", To: "device-b", Payload: payload}); err != nil {
		t.Fatal(err)
	}
	var confirm map[string]string
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	if err := a.ReadJSON(&confirm); err != nil {
		t.Fatal(err)
	}
	if confirm["status"] != "stored" {
		t.Fatalf("store refused: %v", confirm)
	}
	b := dialRelay(t, h)
	mustAuth(t, b, "device-b", privB, pubB)
	// Drain the pending signal first (server drains on connect).
	var delivered SignalMessage
	b.SetReadDeadline(time.Now().Add(2 * time.Second))
	if err := b.ReadJSON(&delivered); err != nil {
		t.Fatal(err)
	}
	if err := b.WriteJSON(SignalMessage{Type: "erase_me", From: "device-b"}); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	if err := b.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["status"] != "forgotten" {
		t.Fatalf("got %v", reply)
	}
	if slots, _ := h.blobs.Count(); slots != 0 {
		t.Fatalf("erase_me left %d blobs", slots)
	}
}

func TestAckUnknownIdIsQuietSuccess(t *testing.T) {
	h := mustHub(t)
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	// Redelivered acks after a crash must not error (or clients retry
	// forever chasing a slot that is already gone).
	ackPayload, _ := json.Marshal(map[string]string{"blob": "1-deadbeefdeadbeef"})
	if err := a.WriteJSON(SignalMessage{Type: "ack", From: "device-a", Payload: ackPayload}); err != nil {
		t.Fatal(err)
	}
	var reply map[string]string
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	if err := a.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["status"] != "acked" {
		t.Fatalf("unknown ack should succeed quietly, got %v", reply)
	}
}

func TestEraseMeForgetsSession(t *testing.T) {
	h := mustHub(t)
	pub, priv := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", priv, pub)
	h.turnKeys.Lock()
	h.turnKeyCache["device-a"] = turnKeyEntry{key: []byte("k"), at: time.Now()}
	h.turnKeys.Unlock()
	if err := a.WriteJSON(SignalMessage{Type: "erase_me", From: "device-a"}); err != nil {
		t.Fatal(err)
	}
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	var reply map[string]string
	if err := a.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if reply["status"] != "forgotten" {
		t.Fatalf("got %v", reply)
	}
	h.mu.RLock()
	_, present := h.devices["device-a"]
	_, pinned := h.pubkeys["device-a"]
	h.mu.RUnlock()
	if present || pinned {
		t.Fatal("session and pin must be gone after erase_me")
	}
	h.turnKeys.Lock()
	_, cached := h.turnKeyCache["device-a"]
	h.turnKeys.Unlock()
	if cached {
		t.Fatal("TURN credential must be gone after erase_me")
	}
}
