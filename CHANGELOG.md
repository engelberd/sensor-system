# Changelog

Wersjonowanie projektu stosuje format `MAJOR.MINOR.PATCH`. Numer wydania jest
wspólny dla narzędzi hosta i firmware'u.

## [0.4.1] - 2026-08-02

### Dodano

- Niezmienna `board_revision` raportowana przez konfigurację, status i
  commissioning noda oraz zapisywana w Capture/Archive.
- Oddzielne presety buildów V1 i V2 oraz kontrola zgodności rewizji przed
  aktualizacją RS485.
- Wspólny lokalny układ `var/recordings`, `var/archive`, `var/diagnostics` i
  `var/log`, wraz z komendą `hostctl paths`.

### Poprawiono

- Build bieżącej instalacji używa profilu `board_v2`; V1 jest budowany w
  osobnym katalogu presetem `board-v1`. Pakiety zapisują profil i liczbową
  rewizję sprzętu.
- Kanoniczne profile `board_v2` i `board_v1` zastępują mylące nazwy historyczne;
  stare nazwy pozostają aliasami zachowującymi swoje dotychczasowe zestawy pinów.
- Skorygowano odwrócone przypisanie zestawów pinów: działające okablowanie
  SCK 14 / MOSI 15 / DRDY 11 / INT1 10 jest teraz jednoznacznie `board_v2`.
- Zamknięte snapshoty i surowe logi diagnostyczne usunięto z drzewa produktu;
  kolejne zrzuty są ignorowane przez Git.
- Usunięto podwójny multicore lockout wokół `flash_safe_execute()`, który mógł
  zatrzymać trial boot na zapisie metadanych i wywołać rollback watchdogiem.
- Sterowanie pojedynczym workerem jest dostępne przez `hostctl channel
  stop/start`, a bootloader respektuje potwierdzony stan zatrzymania supervisora.
- Konfiguracja hosta obsługuje śledzony base i ignorowany lokalny overlay,
  scalając kanały po nazwie i nody po adresie.
- Dodano odczytową komendę `hostctl doctor` sprawdzającą konfigurację, porty,
  sterownik, storage, supervisor, firmware, rewizję płytki i przepływ danych.

## [0.4.0] - 2026-08-02

### Dodano

- Kanoniczny Capture HDF5 v1 z surowym signed-24 w kontenerze `int32`,
  dziennikiem commitów, czasem próbek, niepewnością, konfiguracją i jakością.
- Minimalny Archive HDF5 v1, deterministyczny kompaktor, walidatory oraz
  manifest SHA-256.
- Jawne tożsamości kanałów 1–8 i tymczasowe etykiety sensorów A–H.
- Raportowanie profilu filtra, decymacji, rewizji konfiguracji oraz licznika
  nasyceń kodowania signed-24.

### Poprawiono

- Deduplikacja obejmuje całą sesję Capture i zamknięte okna, dzięki czemu
  retransmisja nie tworzy kopii `late`/`unresolved`.
- Kontrolowane zatrzymanie pozostawia trwające okno UTC jako odzyskiwalny
  `.partial`; restart kontynuuje ten sam plik, a zaległe okna uszczelnia
  atomowo po upływie ich czasu.
- Trwała konfiguracja v3 jest migrowana do v4 bez utraty adresu i ustawień.
- Aktualizacja A/B pozostaje odzyskiwalna, gdy adres serwisowy ma wartość 0.

### Wdrożenie

- Capture v1 działa w trybie fail-closed i pozostaje za przełącznikiem
  `storage.capture_schema`; aktywacja następuje po aktualizacji firmware'u.
- Próba end-to-end na linii D objęła firmware v0.4.0, Capture v1, Archive v1 i
  walidację SHA-256.

## [0.3.10] - 2026-07-15

### Dodano

- Alarm dla czujnika, który odpowiada, ale przestał dostarczać próbki, wraz z
  komunikatem o odzyskaniu przepływu.
- Liczniki węzłów odbierających próbki w dashboardzie i rozszerzony endpoint
  health.
- Kontrolowany restart firmware'u pojedynczego czujnika z dashboardu.
- Skrypt przygotowujący środowisko hosta oraz pełną kontrolę release'u.

### Poprawiono

- Recorder jest ponownie uruchamiany także wtedy, gdy restart węzła się nie uda.
- Testy C++ firmware'u można uruchamiać niezależnie od bieżącego katalogu.
- Ujednolicono numery wersji firmware'u, recordera, supervisora, dashboardu i
  panelu operatora.
- Konfiguracja konkretnej instalacji i lokalne unity systemd nie są częścią
  dystrybucji produktu.

### Znane ograniczenia

- Dashboard i panel operatora nie mają wbudowanego uwierzytelniania; powinny być
  dostępne wyłącznie w zaufanej, chronionej sieci.
- Długotrwała praca nadal wymaga decyzji dotyczącej liczników 32-bitowych i
  jawnej obsługi zawinięcia `uptime_ms` po około 49,7 dnia.
- Diagnostyka elektryczna linii E i przyczyna potwierdzonych utraconych próbek na
  linii G pozostają zadaniami sprzętowymi przed ostatecznym odbiorem instalacji.

## [0.3.9] - 2026-07-11

- Trwała diagnostyka utraty próbek i alarmy po stronie hosta.

## [0.3.8] - 2026-07-10

- Wzmocniona obsługa FIFO i rozszerzona telemetria firmware'u.

## [0.3.0] - 2026-05-24

- Pierwszy pakowany release firmware'u z obrazem fabrycznym i aktualizacją A/B.
