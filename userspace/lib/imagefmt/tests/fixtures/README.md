# imagefmt test fixtures

- `tiny16.jpg` — a 16×16 8-bit grayscale **baseline (SOF0)** JPEG used by
  `jpeg.rs`'s decoder test. It is DC-only (each 8×8 block is a constant
  value; block values `[40, 120, 200, 90]` → a 2×2 pattern of grays, so
  the decoded image is non-uniform), which any conformant baseline decoder
  reconstructs exactly. Encoded with the standard ITU-T T.81 Annex K.3
  luminance Huffman tables and an identity quantization table.

  Regenerate with the committed generator (no external image library
  needed — it is a minimal standard-conformant baseline encoder):

  ```
  python3 tools/mkjpeg.py   # writes tests/fixtures/tiny16.jpg
  ```
