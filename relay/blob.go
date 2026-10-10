package main

// TTL-bound opaque blob slots for off-LAN sync.
//
// A device stores ciphertext addressed to another device; the relay
// persists it (one fsync'd file per blob) until the recipient fetches
// and acks it, or 30 days pass, whichever comes first. The relay never
// parses payloads: they are written and re-emitted byte-identical.
// Duplicates are safe by construction (CRDT re-apply is idempotent),
// so delivery is at-least-once and acks make it effectively-once.
//
// This is the relay's ONLY disk state, and the one deliberate exception
// to the memory-only rule (see docs/STORAGE_CONTRACT.md): bounded,
// expiring, unreadable here, deletable by the owner at any time.

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Blob retention and quota. All local policy (no protocol values):
// clients discover them only through accept/reject responses.
const blobTTL = 30 * 24 * time.Hour
const maxBlobsPerDevice = 100
const maxBlobDiskBytes = int64(1) << 30 // 1 GiB across all recipients

var validRecipient = regexp.MustCompile(`^[A-Za-z0-9_-]{1,128}$`)
var validBlobID = regexp.MustCompile(`^[0-9]+-[0-9a-f]{16}$`)

// storedBlob is one persisted slot. Expires is unix seconds.
type storedBlob struct {
	From    string          `json:"from"`
	To      string          `json:"to"`
	Payload json.RawMessage `json:"payload"`
	Ts      int64           `json:"ts"`
	Expires int64           `json:"expires"`
}

// BlobStore owns the blob directory: one file per blob, tmp+rename
// writes, filename-derived index rebuilt at startup. All methods are
// safe for concurrent use; file IO happens under the store lock so the
// index and the directory can never disagree.
type BlobStore struct {
	mu      sync.Mutex
	dir     string
	pending map[string][]string // recipient -> blob ids, FIFO
	sizes   map[string]int64    // blob id -> file bytes
	total   int64
}

// NewBlobStore opens (creating) dir and rebuilds the index, dropping
// expired or unparseable files. Unparseable files are deleted, never
// served: a corrupt slot must not wedge a recipient's drain.
func NewBlobStore(dir string) (*BlobStore, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, err
	}
	b := &BlobStore{
		dir:     dir,
		pending: make(map[string][]string),
		sizes:   make(map[string]int64),
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	now := time.Now().Unix()
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		name := e.Name()
		// <recipient>.<id>.blob: anything else is not ours; leave it
		// alone (a foreign file in our dir is an operator problem, and
		// deleting unknown files would be data loss by another name).
		if !strings.HasSuffix(name, ".blob") {
			continue
		}
		base := strings.TrimSuffix(name, ".blob")
		dot := strings.LastIndex(base, ".")
		if dot < 0 {
			continue
		}
		to, id := base[:dot], base[dot+1:]
		if !validRecipient.MatchString(to) || !validBlobID.MatchString(id) {
			continue
		}
		raw, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			continue
		}
		var blob storedBlob
		if err := json.Unmarshal(raw, &blob); err != nil {
			os.Remove(filepath.Join(dir, name))
			continue
		}
		if blob.To != to || blob.Expires < now {
			os.Remove(filepath.Join(dir, name))
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		b.pending[to] = append(b.pending[to], id)
		b.sizes[to+"."+id] = info.Size()
		b.total += info.Size()
	}
	for to := range b.pending {
		sort.Strings(b.pending[to])
	}
	return b, nil
}

func blobPath(dir, to, id string) string {
	return filepath.Join(dir, to+"."+id+".blob")
}

func newBlobID() string {
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		// No nonce, no slot: predictable ids would let a sender
		// overwrite (and a reader guess) another device's blobs.
		return ""
	}
	return strconv.FormatInt(time.Now().UnixNano(), 10) + "-" + hex.EncodeToString(nonce[:])
}

// Store persists one opaque payload for `to`. Returns the blob id, or a
// short machine-readable refusal ("quota exceeded"). Payload size is
// already bounded by the connection read limit; this enforces count and
// disk quotas on top.
func (b *BlobStore) Store(from, to string, payload json.RawMessage, ts int64) (string, string) {
	if !validRecipient.MatchString(to) || len(payload) == 0 {
		return "", "invalid recipient"
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	now := time.Now().Unix()
	// Sweep-then-count: expired slots free before quota is judged, so a
	// recipient at cap with only dead blobs still gets room.
	b.sweepLocked(now)
	if len(b.pending[to]) >= maxBlobsPerDevice {
		return "", "quota exceeded"
	}
	id := newBlobID()
	if id == "" {
		return "", "internal error"
	}
	blob := storedBlob{
		From:    from,
		To:      to,
		Payload: payload,
		Ts:      ts,
		Expires: now + int64(blobTTL/time.Second),
	}
	raw, err := json.Marshal(blob)
	if err != nil {
		return "", "internal error"
	}
	if b.total+int64(len(raw)) > maxBlobDiskBytes {
		return "", "quota exceeded"
	}
	tmp, err := os.CreateTemp(b.dir, ".tmp-*.blob")
	if err != nil {
		return "", "internal error"
	}
	tmpName := tmp.Name()
	if _, err := tmp.Write(raw); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return "", "internal error"
	}
	// Fsync before rename: a crash between write and rename must leave
	// either the complete blob or nothing, never a prefix.
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return "", "internal error"
	}
	tmp.Close()
	dst := blobPath(b.dir, to, id)
	if err := os.Rename(tmpName, dst); err != nil {
		os.Remove(tmpName)
		return "", "internal error"
	}
	b.pending[to] = append(b.pending[to], id)
	b.sizes[to+"."+id] = int64(len(raw))
	b.total += int64(len(raw))
	return id, ""
}

// PendingBlob is one live slot with its server-assigned id (the value
// the recipient acks to delete it).
type PendingBlob struct {
	ID   string
	Blob storedBlob
}

// Pending returns the recipient's live blobs oldest-first (sweeping
// expired ones first so drains never serve rot).
func (b *BlobStore) Pending(to string) []PendingBlob {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.sweepLocked(time.Now().Unix())
	ids := b.pending[to]
	out := make([]PendingBlob, 0, len(ids))
	for _, id := range ids {
		raw, err := os.ReadFile(blobPath(b.dir, to, id))
		if err != nil {
			continue
		}
		var blob storedBlob
		if err := json.Unmarshal(raw, &blob); err != nil || blob.To != to {
			continue
		}
		out = append(out, PendingBlob{ID: id, Blob: blob})
	}
	return out
}

// Ack deletes one delivered blob. Only the recipient's own slot can be
// removed (id and recipient both validated): one device can never ack
// away another's mail.
func (b *BlobStore) Ack(to, id string) bool {
	if !validRecipient.MatchString(to) || !validBlobID.MatchString(id) {
		return false
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	key := to + "." + id
	size, ok := b.sizes[key]
	if !ok {
		return false
	}
	if err := os.Remove(blobPath(b.dir, to, id)); err != nil && !os.IsNotExist(err) {
		return false
	}
	delete(b.sizes, key)
	b.total -= size
	kept := b.pending[to][:0]
	for _, kept_id := range b.pending[to] {
		if kept_id != id {
			kept = append(kept, kept_id)
		}
	}
	if len(kept) == 0 {
		delete(b.pending, to)
	} else {
		b.pending[to] = kept
	}
	return true
}

// ForgetRecipient drops every blob addressed to `to` (erase_me path).
// Returns the count for the log line (truncated id only, as everywhere).
func (b *BlobStore) ForgetRecipient(to string) int {
	if !validRecipient.MatchString(to) {
		return 0
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	n := 0
	for _, id := range b.pending[to] {
		key := to + "." + id
		if size, ok := b.sizes[key]; ok {
			b.total -= size
			delete(b.sizes, key)
		}
		os.Remove(blobPath(b.dir, to, id))
		n++
	}
	delete(b.pending, to)
	return n
}

// sweepLocked deletes expired slots. Caller must hold mu.
func (b *BlobStore) sweepLocked(now int64) {
	for to, ids := range b.pending {
		kept := ids[:0]
		for _, id := range ids {
			key := to + "." + id
			raw, err := os.ReadFile(blobPath(b.dir, to, id))
			expired := err != nil
			if !expired {
				var blob storedBlob
				if err := json.Unmarshal(raw, &blob); err != nil || blob.Expires < now {
					expired = true
				}
			}
			if expired {
				if size, ok := b.sizes[key]; ok {
					b.total -= size
					delete(b.sizes, key)
				}
				os.Remove(blobPath(b.dir, to, id))
				continue
			}
			kept = append(kept, id)
		}
		if len(kept) == 0 {
			delete(b.pending, to)
		} else {
			b.pending[to] = kept
		}
	}
}

// Sweep drops expired slots. Safe to call on any cadence; delivery and
// store paths sweep their own recipient first, so this is backstop only.
func (b *BlobStore) Sweep() {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.sweepLocked(time.Now().Unix())
}

// Count returns live slot totals (for tests and operator endpoints).
func (b *BlobStore) Count() (slots int, bytes int64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	for _, ids := range b.pending {
		slots += len(ids)
	}
	return slots, b.total
}
