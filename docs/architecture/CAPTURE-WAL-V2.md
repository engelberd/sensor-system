# Capture WAL v2

- Status: zaimplementowane; 2026-11-02; [ADR-0012](../adr/0012-individual-sample-timestamps.md)

Envelope, record types, CRC32C, durability, rotacja i recovery zachowują reguły
[WAL v1](CAPTURE-WAL-V1.md), z poniższą jawną zmianą formatu:

- wersja pliku w nagłówku wynosi **2**; writer i reader odrzucają inne wersje;
- record version pozostaje 1 w obrębie nowego file version;
- SampleChunk payload: dotychczasowe 56 B metadanych, `9*N` bajtów XYZ24,
  następnie **N timestampów u64 LE** (8*N B);
- timestampy są ściśle rosnące, ich liczba odpowiada XYZ, a skrajne wartości
  odpowiadają kotwicom; odstępy mieszczą się w u32 mikrosekund;
- CRC pokrywa również wszystkie timestampy; completion i Release następują
  dopiero po tej samej barierze zapisu co XYZ;
- replay i eksport nigdy nie interpolują czasu próbek.

Pliki v1 pozostają oddzielnymi artefaktami poprzedniej wersji. Aktualizacja
wymaga nowego katalogu Capture, a nie dopisywania do starych segmentów.
Dane czasu oznaczają czas monotoniczny Pico po przypisaniu przez filtr, a nie UTC.

## Wspólne granice WAL/CSV

Politykę rotacji z WAL v1 zastępuje [ADR-0013](../adr/0013-shared-capture-calendar.md):
granice kalendarzowe, podział chunku między próbkami i zamknięcie przed nową
rewizją konfiguracji. Każdy nowy segment posiada atomowo utrwalony sidecar
`window-<ordinal>.calendar.json` z CRC32C i zamrożoną kotwicą czasu. Format
binarny pozostaje v2. Sidecar podlega tej samej retencji co WAL i manifest.
