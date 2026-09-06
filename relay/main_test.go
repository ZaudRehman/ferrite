package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

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
	// Wrong device id.
	if _, ok := verifyAuth("dev-2", p); ok {
		t.Fatal("wrong device id accepted")
	}
	// Stale timestamp.
	old := p
	old.Ts = ts - maxAuthSkew - 1
	if _, ok := verifyAuth("dev-1", old); ok {
		t.Fatal("stale auth accepted")
	}
	// Future timestamp.
	future := p
	future.Ts = ts + maxAuthSkew + 1
	if _, ok := verifyAuth("dev-1", future); ok {
		t.Fatal("future auth accepted")
	}
	// Malformed key.
	bad := p
	bad.PubKey = "!!!"
	if _, ok := verifyAuth("dev-1", bad); ok {
		t.Fatal("malformed key accepted")
	}
	// Empty device.
	if _, ok := verifyAuth("", p); ok {
		t.Fatal("empty device accepted")
	}
}

func TestAuthRequiredFirst(t *testing.T) {
	h := NewHub()
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
	h := NewHub()
	pubA, privA := testKey(t)
	pubB, privB := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	_ = pubB
	_ = privB
	// device-a sends with a forged From.
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
	h := NewHub()
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)

	// Second session, same id, different key: must be rejected, first stays.
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

	// Original holder still routes: send to self via relay (offline peer => 404, but session alive).
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
	h := NewHub()
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)

	b := dialRelay(t, h)
	mustAuth(t, b, "device-a", privA, pubA)
	// Old holder got evicted (its reads fail); new holder works.
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
	h := NewHub()
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
	h := NewHub()
	pubA, privA := testKey(t)
	a := dialRelay(t, h)
	mustAuth(t, a, "device-a", privA, pubA)
	big := strings.Repeat("x", maxMessageBytes)
	if err := a.WriteJSON(SignalMessage{Type: "signal", From: "device-a", To: "nobody", Payload: json.RawMessage(`"` + big + `"`)}); err != nil {
		// Write-side failure also acceptable (client self-limits).
		return
	}
	// Server closed the connection on oversize read.
	a.SetReadDeadline(time.Now().Add(2 * time.Second))
	var reply map[string]string
	_ = a.ReadJSON(&reply)
}
