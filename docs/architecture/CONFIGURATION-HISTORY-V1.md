# Configuration History v1

- Status: obowiązujący kontrakt trwałości hosta v2
- Ostatnia aktualizacja: 2026-10-15

Capture zapisuje przy próbce `config_revision`, ale sama liczba nie opisuje
ustawień sensora. Host utrzymuje dlatego mały, osobny WAL snapshotów
konfiguracji. Nie leży on na gorącej drodze próbek: zapis następuje podczas
bootstrapu strumienia oraz po udanym Apply/Reset, zanim worker przyjmie nowy
plan do pamięci.

## Tożsamość i granica obowiązywania

Jeden rekord zawiera:

- `hardware_id` i `boot_epoch` strumienia;
- pełny `ObservedConfiguration` wraz z revision, ODR sensora i decymacją;
- `effective_sample_seq` zwrócony przez node.

Konfiguracja dotyczy próbek tej samej tożsamości strumienia od
`effective_sample_seq` do następnego rekordu. Rekord bootstrapu dokumentuje stan
zastany przed otwarciem runtime. Analiza łączy go z `config_revision` zapisanym
w każdym chunku Capture. Nie wolno zgadywać konfiguracji wyłącznie z profilu
instalacji.

## Format i recovery

Pliki znajdują się w `.configuration-v1/<hardware_id>.wal` pod katalogiem
Capture. Rekord ma stałe 56 bajtów, magic `CFG1`, wersję schematu 1 i CRC-32C.
Append kończy się `sync_data`; utworzenie pliku obejmuje synchronizację katalogu.

Recovery może usunąć jedynie niepełny rekord końcowy. Błędne magic, pola
zarezerwowane, konfiguracja albo CRC kompletnego rekordu zatrzymują start.
Powtórzenie identycznej pary `(boot_epoch, revision)` jest idempotentne.
Ta sama para z inną konfiguracją jest konfliktem integralności.

## Granica odpowiedzialności

Ten WAL jest dowodem znaczenia danych pomiarowych. Nie jest audytem operatora:
nie przechowuje użytkownika, treści żądania ani nieudanych operacji. Trwały
audit Operations API pozostaje osobnym modułem do późniejszej implementacji.
