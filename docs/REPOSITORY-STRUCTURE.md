# Wspólna struktura repozytorium

Repozytorium jest jednym monorepo dla elektroniki, firmware'u i oprogramowania
hosta. Wszystkie instalacje pracują na `main`; nie tworzymy osobnych gałęzi dla
hostów ani rewizji płytek.

```text
sensor-system/
├── hardware/                 KiCad, biblioteki i dokumentacja elektroniki
├── node/                     firmware, bootloader i profile board-v1/board-v2
├── host/
│   ├── configs/
│   │   ├── common.json       wspólne ustawienia produktu
│   │   └── systems/          śledzone profile fizycznych instalacji
│   ├── common/               wspólny model konfiguracji i statusu
│   ├── recorder/             zapis Capture i Archive
│   ├── systemd/              wyłącznie szablony jednostek
│   ├── tests/                testy hosta
│   └── system_config.json    lokalna, ignorowana nakładka aktywnego hosta
├── docs/                     formaty danych, ADR-y i instrukcje
├── tools/                    wspólne narzędzia repozytorium
└── var/                      lokalne dane robocze; zawartość ignorowana
    ├── recordings/
    ├── archive/
    ├── diagnostics/
    ├── log/
    └── tmp/
```

## Trzy poziomy konfiguracji

1. `host/configs/common.json` — format zapisu, katalogi względne, rotacja logów
   i zachowanie supervisora wspólne dla wszystkich instalacji.
2. `host/configs/systems/<system>.json` — śledzony w Git opis instalacji:
   kanały, baudrate, rewizje płytek i oczekiwane ustawienia czujników.
3. `host/system_config.json` — ignorowana nakładka jednego komputera: porty
   `/dev`, absolutne ścieżki, nazwa hosta oraz chwilowo wyłączone kanały.

Profile mogą dziedziczyć po sobie przez pole `extends`. Ścieżka jest liczona
względem pliku, który ją deklaruje. Nowy system zaczynamy od skopiowania
`host/configs/systems/system.example.json` pod jego docelową nazwę.
Nowy host inicjalizujemy poleceniem `./host/tools/setup_host.sh <nazwa-profilu>`.

## Zasady Gita

- Commitujemy kod, testy, dokumentację, źródła KiCad i stabilne profile systemów.
- Nie commitujemy nagrań, logów, środowiska `.venv`, buildów, aktywnych jednostek
  systemd ani `host/system_config.json`.
- Rewizja płytki należy do konkretnego węzła w profilu systemu. Mapa pinów
  pozostaje w `node/config/board_profile.h` i jest wybierana podczas budowania.
- Po pobraniu zmian uruchamiamy `./hostctl paths --init`, a następnie
  `./hostctl doctor`.
