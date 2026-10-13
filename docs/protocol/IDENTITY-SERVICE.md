# Application protocol 5 — Identity Service

- Status: obowiązujący kontrakt wire
- Date: 2026-09-29
- Service ID: `1`

## 1. Handshake

Przed recording host wykonuje kolejno `GET_IDENTITY` i `GET_CAPABILITIES`.
Oba requesty mają pusty payload, są `RetrySafe` i używają zwykłej polityki
deadline z [TRANSACTION-POLICY.md](TRANSACTION-POLICY.md).

Host odmawia rozpoczęcia sesji, gdy nie zgadza się framing/application version,
tożsamość instalacji, board revision albo wymagane capabilities. Port USB i
adres RS485 nie są trwałą tożsamością urządzenia.

## 2. Wspólny status odpowiedzi

Pierwszy bajt każdego response application protocol 5 jest statusem:

| Kod | Nazwa |
| ---: | --- |
| 0 | `OK` |
| 1 | `MALFORMED_REQUEST` |
| 2 | `UNSUPPORTED_SERVICE` |
| 3 | `UNSUPPORTED_MESSAGE` |
| 4 | `WRONG_MODE` |
| 5 | `BUSY` |
| 6 | `CONFLICT` |
| 7 | `INVALID_STATE` |
| 8 | `INTERNAL_ERROR` |

Response błędu zawiera obecnie tylko status. Status nieznany dla application
protocol 5 jest błędem kompatybilności, a nie sukcesem.

## 3. `GET_IDENTITY` — message 1

Success response ma dokładnie 24 bajty:

| Offset | Bajty | Pole |
| ---: | ---: | --- |
| 0 | 1 | `status = OK` |
| 1 | 8 | `hardware_id`, niezmienne bajty Pico unique ID |
| 9 | 8 | `boot_epoch u64 LE`, niezerowe |
| 17 | 1 | `node_address`, 1..253; 254 jest zarezerwowane dla Commissioning |
| 18 | 1 | `board_revision`, niezerowe |
| 19 | 3 | firmware major, minor, patch |
| 22 | 1 | framing version, obecnie 3 |
| 23 | 1 | application version, obecnie 5 |

`boot_epoch` powstaje przy każdym starcie aplikacji i identyfikuje ciągłość
strumienia wraz z hardware ID. Późniejsza decyzja o osobnym restarcie acquisition
może wprowadzić nowy stream epoch przez wersjonowaną zmianę Measurement Service.

## 4. `GET_CAPABILITIES` — message 2

Success response ma dokładnie 24 bajty:

| Offset | Bajty | Pole |
| ---: | ---: | --- |
| 0 | 1 | `status = OK` |
| 1 | 1 | capabilities schema, obecnie 2 |
| 2 | 4 | service bitmap `u32 LE`; bit `service_id - 1` |
| 6 | 8 | feature bitmap `u64 LE` |
| 14 | 2 | maksymalny payload framingu |
| 16 | 4 | fizyczna pojemność SampleJournal w próbkach |
| 20 | 2 | maska output ODR Measurement Service |
| 22 | 2 | maksymalna liczba próbek jednego chunku |

Output-rate bits są stabilną tabelą dokładnych częstotliwości wyjściowych:

| Bit | Output ODR |
| ---: | ---: |
| 0 | 4000 Hz |
| 1 | 2000 Hz |
| 2 | 1000 Hz |
| 3 | 500 Hz |
| 4 | 250 Hz |
| 5 | 125 Hz |
| 6 | 62.5 Hz |
| 7 | 31.25 Hz |
| 8 | 15.625 Hz |
| 9 | 7.8125 Hz |
| 10 | 3.90625 Hz |

Maska opisuje próbki publikowane przez Measurement Service po obowiązkowym
filtrowaniu i decymacji. Nie opisuje rejestru ani wejściowego ODR sensora.
Obecny firmware przyjmuje sensor ODR `4000, 2000, 1000, 500, 250, 125 Hz` i
zawsze decymuje ×2, dlatego uczciwa maska wynosi `0x007e`: output
`2000, 1000, 500, 250, 125, 62.5 Hz`. Host używa właśnie tych wartości do
schedulera i admission control.

Feature bits schema 2:

| Bit | Znaczenie |
| ---: | --- |
| 0 | bounded `SampleJournal` z retransmisją |
| 1 | timestampy związane z DRDY |
| 2 | atomowa konfiguracja z revision |
| 3 | jawne potwierdzanie trial boot |
| 4 | zdalny update firmware przez bootloader A/B |

Jeżeli node deklaruje Measurement Service, musi również deklarować journal,
niezerową pojemność, przynajmniej jeden output ODR i niezerowy limit chunku. Host nie
uzupełnia brakujących capabilities domysłami.

Walidacja schema 2 jest fail-closed. Odrzuca nieznane bity services, features i
output rates, brak Identity Service, niespójną parę remote-update/trial-confirm
oraz chunk, który nie mieści się w deklarowanym payloadzie lub journalu. Host
może też dołączyć wymagany output rate do handshake; node bez tego rate nie
zostanie dopuszczony do sesji.

Feature bits 3 i 4 wystawia wyłącznie obraz uruchamiany w slocie A/B. Samodzielny
obraz integracyjny nie może reklamować trial confirm ani zdalnego update, ponieważ
nie ma pod sobą zachowanego bootloadera i metadanych slotów.

## 5. Własność i rozszerzanie

Identity opisuje fakty jednego uruchomienia. Capabilities opisują ograniczenia
konkretnego firmware, obrazu i sprzętu. Bieżący output ODR jest observed state
konfiguracji, a wybrany output ODR jest desired state hosta; żadnego z nich nie
udajemy bitmapą capabilities. Odczyt tej pary trafi do Configuration Service.
Dodanie pola zmienia schema albo application protocol; nie wydłużamy po cichu
stałego payloadu. Schema 1 jest odrzucana przez nowy host, ponieważ jej pole ODR
miało inną semantykę i nie pozwalało bezpiecznie wyznaczyć output rate.
