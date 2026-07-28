# Raport L1 timestampingu — linia D, node 1

Data: 2026-07-27
System: testowy
Zakres: jeden ADXL355, `/dev/ttyCH9344USB3`, node `1`

## Konfiguracja i obraz

- profil sprzętowy: `legacy_eval`;
- piny: DRDY GPIO11, INT1 GPIO10, SPI1 GPIO12–15;
- sensor ODR: 250 Hz;
- output ODR po FIR x2: 125 Hz;
- firmware przed testem: v0.3.9;
- firmware po teście: v0.3.10, timestamping v2 ON;
- capabilities: wersja 1, Burst max 2, flags `0x0F`, 32 próbki/pakiet;
- aktywny obraz końcowy: slot B.

SHA-256 końcowych artefaktów:

- slot A:
  `cd9e885325f10a1e88e76d6d93b79bb34e77ccb56b05869423ac9ca63a240b6d`;
- slot B:
  `cced6a11a145f7f4153a01b25d4e4a3917f2015f9a1f9cdafe6f37af1ccf5c43`;
- manifest:
  `cee951c47236d5f4a4565ff5ed7bc94ceb70f3fa900116d8d0dc7497aa8b6212`.

## TimeSync

Model uzyskał `LOCKED` po pięciu obserwacjach. W krótkiej próbie:

- slope device→host: 999,988 ns/µs;
- różnica względem ideału: około −12 ppm;
- RTT pojedynczego odpytywania: około 30–50 ms;
- maksymalna niepewność zapisana w kotwicach: 15,8 ms;
- jedna stabilna `boot_epoch` w każdym przebiegu.

## Test observe — 120 s

Artefakt:
`runs/manual-tests/line-d-timing-observe/2026-07-27/`

- 14 976 próbek;
- 468 kotwic;
- 8 obserwacji synchronizacji;
- sequence bez przerw;
- zero gapów, nowych strat, overflow ringa i recovery podczas sesji;
- jedna epoka i jeden stabilny segment (`3`);
- okres próbki: min 8000,065 µs, średnio 8000,581 µs,
  max 8001,097 µs;
- HDF5 schema 5, `complete=true`.

## Test required — 90 s

Artefakty:
`runs/manual-tests/line-d-timing-required/`

- 11 264 próbki łącznie, sequence bez przerw;
- 2 560 początkowych próbek bez locka w trwałej kwarantannie `unsynced`;
- 8 704 zsynchronizowane próbki w zwykłym oknie 22:50–23:00;
- 272/272 kotwice zwykłego okna mają UTC akwizycji;
- oba HDF5 mają schema 5 i `complete=true`;
- zero gapów i nowych strat podczas sesji.

Tryb `required` został rozszerzony o osobne pliki kwarantanny dla:

- `unsynced` — brak pewnego mapowania UTC;
- `ambiguous` — przedział niepewności przecina granicę okna;
- `late` — okno zostało już sfinalizowane.

Próbki z tych kategorii nie trafiają arbitralnie do zwykłych okien, ale po
trwałym zapisie mogą zostać bezpiecznie commitowane do noda.

## Obserwacja wymagająca dalszego testu

Po każdym restarcie noda bariera fail-closed odrzucała 10–15 próbek i
wykonywała 1–2 recovery, po czym utrzymywała stabilny segment bez dalszego
wzrostu liczników. Próba zmiany kolejności uzbrajania IRQ i jawnego czyszczenia
FIFO nie poprawiła wyniku, dlatego została cofnięta.

Nie znaleziono utraty danych po wejściu w stabilny segment, ale przed rolloutem
na kolejne nody należy zarejestrować DRDY, INT1 i transakcje FIFO analizatorem
logicznym podczas pierwszych sekund startu. Do tego czasu startowy warm-up jest
jawnie kwarantannowany przez firmware i host.

## Wynik

L1 dla pojedynczego noda: warunkowo pozytywny.

- Burst v2, TimeSync, modele zegara, `observe`, `required`, HDF5 v5,
  deduplikacja i kwarantanna działają na rzeczywistym czujniku.
- Nie rozszerzać rollout’u na kolejne nody przed analizą startowego recovery.
