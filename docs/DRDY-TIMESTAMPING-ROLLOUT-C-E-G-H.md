# Rollout timestampingu — linie C, E, G i H

Data: 2026-07-27  
System: testowy, rejestracja bez określonego czasu zakończenia

## Zakres

- A i B pozostawiono bez zmian jako kanały referencyjne w trybie `legacy`.
- D pozostaje kanałem L1 w trybie `required`.
- C, E, G i H zaktualizowano z v0.3.9 do v0.3.10 z timestampingiem v2.
- F jest fizycznie odłączona i została wyłączona w lokalnej konfiguracji hosta.

Wszystkie aktualizowane nody używają profilu `legacy_eval`:

- DRDY: GPIO11;
- INT1: GPIO10;
- SPI1: GPIO12–15.

## Obraz

Konfiguracja kompilacji:

```text
CMAKE_BUILD_TYPE=Release
SENSOR_SYSTEM_BOARD_PROFILE=legacy_eval
SENSOR_SYSTEM_ENABLE_TIMESTAMPING_V2=ON
```

SHA-256:

- slot A:
  `cd9e885325f10a1e88e76d6d93b79bb34e77ccb56b05869423ac9ca63a240b6d`;
- slot B:
  `cced6a11a145f7f4153a01b25d4e4a3917f2015f9a1f9cdafe6f37af1ccf5c43`;
- pakiet aktualizacji:
  `cee951c47236d5f4a4565ff5ed7bc94ceb70f3fa900116d8d0dc7497aa8b6212`.

Są to te same artefakty, które przeszły L1 na linii D.

## Konfiguracja po aktualizacji

| Linia | Firmware | Sensor ODR | Output ODR | Routing |
|---|---:|---:|---:|---|
| A | v0.3.0 | 250 Hz | 125 Hz | `legacy` |
| B | v0.3.0 | 250 Hz | 125 Hz | `legacy` |
| C | v0.3.10 | 500 Hz | 250 Hz | `required` |
| D | v0.3.10 | 250 Hz | 125 Hz | `required` |
| E | v0.3.10 | 250 Hz | 125 Hz | `required` |
| F | odłączona | — | — | wyłączona |
| G | v0.3.10 | 250 Hz | 125 Hz | `required` |
| H | v0.3.10 | 250 Hz | 125 Hz | `required` |

Po zmianie slotu jawnie odtworzono i zapisano konfigurację każdego
aktualizowanego noda. C zachowała 500 Hz; E, G i H zachowały 250 Hz.
Po czystym restarcie wszystkie cztery nody raportowały:

- `fifo_overruns=0`;
- `drdy_ring_ovf=0`;
- `timing_mismatch=0`;
- `timing_invalid=0`;
- stabilny segment czasu.

## Host

Supervisor uruchamia:

- A i B z `--timing-mode legacy`;
- C, D, E, G i H z `--timing-mode required`;
- bez workera dla F.

Po świeżym restarcie supervisora i dashboardu:

- 7/7 aktywnych nodów było online;
- 7/7 dostarczało próbki;
- `gaps_detected=0`;
- nowe straty, overflow i packet overwrite w sesji wynosiły zero;
- dashboard raportował `system_healthy=true` i `attention_count=0`.

Początkowe próbki przed lockiem TimeSync oraz próbki niejednoznaczne na
granicy okna są trwale kierowane do kwarantanny. Zwykłe HDF5 v5 są kierowane
według czasu akwizycji.

## Znane ograniczenie

Na wszystkich badanych nodach pozostaje obserwowany wzorzec nadmiarowych
zboczy INT1: około połowa zdarzeń występuje poniżej watermarku FIFO.
Nie powoduje to obecnie przepełnień ani utraty danych, ponieważ firmware
weryfikuje stan FIFO i ma fallback pollingowy. Test długotrwały ma pozostać
uruchomiony bez określonego czasu zakończenia.
