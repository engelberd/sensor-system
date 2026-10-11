# Golden protocol vectors

Ten katalog będzie zawierał wersjonowane ramki binarne wraz z oczekiwanym
znaczeniem. Te same wektory muszą przechodzić w testach Rust hosta i C++ node.

Wektory framing v3:

- `framing_v3_request_empty.bin` — pusty request host → node 7;
- `framing_v3_response_mixed.bin` — response z zerami w payloadzie;
- `framing_v3_request_max.bin` — maksymalny payload 512 B.

Pliki generuje niezależny, mały skrypt `generate_framing_v3.py` bez zależności
od implementacji Rust ani C++. Ich znaczenie wynika z
[FRAMING-V3-SPEC.md](../../../docs/protocol/FRAMING-V3-SPEC.md).

Wektory application protocol 5:

- `identity_v5_response.bin` — pełna tożsamość node i wersje;
- `capabilities_v5_response.bin` — schema 2, ograniczenia services, journala i
  output ODR po decymacji.

Generuje je niezależny `generate_identity_v5.py` zgodnie z
[IDENTITY-SERVICE.md](../../../docs/protocol/IDENTITY-SERVICE.md).

Wektor Time Service protocol 5:

- `time_v5_observe_response.bin` — `boot_epoch`, `t2` i `t3` dla krótkiej
  obserwacji zegara.

Generuje go niezależny `generate_time_v5.py` zgodnie z
[TIME-WIRE-V5.md](../../../docs/protocol/TIME-WIRE-V5.md).

Wektory Measurement protocol 5:

- `measurement_v5_pull_request.bin` i `measurement_v5_release_request.bin`;
- `measurement_v5_stream_state_response.bin`;
- `measurement_v5_chunk_response.bin` z granicami signed XYZ24;
- osobne response dla `NO_DATA`, `RANGE_UNAVAILABLE`, `STREAM_CHANGED` i
  `INVALID_RELEASE`;
- `measurement_v5_gap_response.bin` — dokładny brak `20..23` z przyczyną
  `SOURCE_LOSS`.

Generuje je niezależny `generate_measurement_v5.py` zgodnie z
[MEASUREMENT-WIRE-V5.md](../../../docs/protocol/MEASUREMENT-WIRE-V5.md).
