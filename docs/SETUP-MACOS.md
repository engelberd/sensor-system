# Stanowisko macOS: KiCad, firmware i host

Mac może być pełnym stanowiskiem developerskim dla KiCad, firmware'u i
narzędzi hosta uruchamianych w terminalu. Produkcyjne usługi `systemd`, reguły
`udev` i sterowniki kernela Linux pozostają na komputerach Linux.

## 1. Zależności systemowe

Repo deklaruje narzędzia, ale ich nie vendoryzuje. Po zainstalowaniu Homebrew:

```bash
brew bundle --file Brewfile
```

Instalowane są KiCad, Git, CMake, Ninja, Python, `arm-none-eabi-gcc` i
`picotool`. KiCad jest oficjalnie dostępny dla macOS, a Homebrew dostarcza
gotowe pakiety narzędzi ARM i picotool:

- https://www.kicad.org/download/macos/
- https://formulae.brew.sh/cask/kicad
- https://formulae.brew.sh/formula/arm-none-eabi-gcc
- https://formulae.brew.sh/formula/picotool

Sprawdzenie stanowiska jest odczytowe:

```bash
./tools/check_macos.sh
```

Biblioteki hosta mają zgodne zakresy wersji w
`host/requirements-recorder.txt`; nie instalujemy ich globalnie, tylko w
ignorowanym `host/.venv`.

## 2. Pico SDK poza repozytorium

Projekt jest zweryfikowany z wersją zapisaną w `node/PICO_SDK_VERSION`. SDK
trzymamy poza repo, przykładowo:

```bash
mkdir -p "$HOME/Developer"
git clone --branch 2.2.0 --depth 1 \
  https://github.com/raspberrypi/pico-sdk.git \
  "$HOME/Developer/pico-sdk"
export PICO_SDK_PATH="$HOME/Developer/pico-sdk"
```

Eksport można dodać do `~/.zprofile`. Oficjalny Pico SDK obsługuje wskazanie
zewnętrznej lokalizacji przez `PICO_SDK_PATH`:
https://github.com/raspberrypi/pico-sdk

Budowanie pozostaje takie samo na Intel i Apple Silicon:

```bash
cmake -S node --preset board-v2
cmake --build --preset board-v2
```

Artefakty powstają w ignorowanym `node/build/`. Dla V1 używamy osobnego presetu
`board-v1` i katalogu `node/build.v1`.

## 3. Python i lokalna konfiguracja hosta

```bash
./host/tools/setup_host.sh rpi-sanok
cp host/configs/host_system.macos.example.json host/system_config.json
./hostctl paths --init
```

Przed uruchomieniem zmień `extends`, jeśli pracujesz z innym systemem, wpisz
rzeczywiste porty `/dev/cu.*` i włącz tylko fizycznie podłączone kanały.
Szablon używa `var/run` zamiast linuksowego `/run/sensor-system`.

Lista portów z identyfikatorami USB:

```bash
host/.venv/bin/python -m serial.tools.list_ports -v
```

Narzędzia `ping`, `config`, `recorder`, `supervisor`, `dashboard` i `operator`
mogą działać lokalnie. Supervisor i panele uruchamiamy na Macu w terminalu;
sterowanie usługami `systemd` jest funkcją produkcyjnego hosta Linux.

## 4. USB-RS485 i CH9344

Nazwa handlowa „USB-RS485” nie określa sterownika — decyduje chipset. Po
podłączeniu konwertera sprawdź:

```bash
system_profiler SPUSBDataType
host/.venv/bin/python -m serial.tools.list_ports -v
```

Obecny ośmiokanałowy konwerter Sanoka używa CH9344. Oficjalny katalog WCH nie
udostępnia sterownika macOS dla CH9344, a pakiet CH341SER_MAC nie wymienia tego
układu. Nie instalujemy przypadkowych, niepodpisanych sterowników z kopii w
Internecie:

- https://www.wch-ic.com/downloads/category/67.html
- https://www.wch-ic.com/downloads/CH341SER_MAC_ZIP.html

Mamy dwa obsługiwane warianty:

1. Do lokalnych testów użyć konwertera tworzącego port `/dev/cu.*` na macOS
   (najlepiej standard USB CDC albo chipset z oficjalnym sterownikiem dla
   używanej wersji macOS).
2. Zostawić CH9344 podłączony do Raspberry Pi/Linux i pracować z Maca przez Git,
   SSH oraz panel WWW hosta. To jest bezpieczniejszy wariant dla pełnych ośmiu
   kanałów.

Jeśli po podłączeniu CH9344 nie powstaną wszystkie porty `/dev/cu.*`, problemu
nie rozwiązuje zmiana konfiguracji repo — potrzebny jest kompatybilny sterownik
lub inny konwerter.

## 5. KiCad

Otwieraj projekt `hardware/KiCad - node V2/RP2350.kicad_pro`. Źródła projektu i
lokalne biblioteki są wersjonowane; cache, backupy i wygenerowane pliki
produkcyjne są ignorowane. Na macOS `kicad-cli` znajduje się wewnątrz pakietu
aplikacji pod `/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli`.

Przed rozpoczęciem pracy wykonaj `git pull`, a przed commitem sprawdź, czy nie
dodajesz backupów, Gerberów ani lokalnych tabel bibliotek.
