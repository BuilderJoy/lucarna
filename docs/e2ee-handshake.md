# End-to-end handshake (Lucarna E2EE, wire `LUCE2E02`)

The host and the person helping share a short PIN. The PIN is the *password* of a SPAKE2 exchange, so the
PIN never travels and a recording of the exchange cannot be used to try PINs at home. The repeater only
moves bytes it cannot read.

```mermaid
sequenceDiagram
    autonumber
    participant H as Host (side A)
    participant R as Repeater (cannot read)
    participant V as Viewer (side B)

    Note over H,V: Both know the 6-digit PIN. Neither sends it.
    H->>R: register ID:code
    V->>R: connect ID:code
    R-->>H: pairs the sockets
    H->>V: "LUCE2E02" + X = x·G + w·M   (33 bytes)
    Note over V: y random, Y = y·G + w·N,  K = y·(X - w·M)
    V->>H: Y (33 bytes) + proof = HMAC(K, "viewer")  (32 bytes)
    Note over H: K = x·(Y - w·N),  checks the proof in constant time
    alt proof wrong
        H-->>V: 0xEE (or 0xFF at the 3rd failure: locked)
        Note over H: failures + 1, PIN replaced, 3 failures end the share
    else proof right
        H->>V: HMAC(K, "host")  (32 bytes)
        Note over H,V: directional keys from K, ChaCha20-Poly1305 frames
        H->>H: Allow / Deny question (person sharing)
        H->>V: RFB screen, input, files (encrypted)
    end
```

`w` is a hash of the PIN, `G` the group generator, `M` and `N` fixed public points. A wrong PIN gives a
different `K` that looks random, so the proof fails and nothing about the PIN leaks.

What stays true: a live attacker still gets one guess per connection (the host counts three), and the viewer
has no lock of its own. What is gone: guessing offline from a recording.
