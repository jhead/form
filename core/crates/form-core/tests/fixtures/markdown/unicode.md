Ranges are UTF-16 code units, not bytes.

```rust
let party = "🎉 日本語 café";
let plain = "ascii";
```

```
🎉 日本語 café with no language
```

Prose with 日本語, an emoji 🎉, and a [link](https://example.com/日本語).
