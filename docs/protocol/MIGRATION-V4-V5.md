# Migracja application protocol 4 → 5 wyłącznie przez RS485

- Status: obowiązujący warunek wdrożenia
- Dotyczy: zainstalowanych node z bootloader protocol 2 i układem A/B
- Factory/USB: niewymagane dla obecnie planowanej migracji

## 1. Niezmienne granice

Pierwsza wersja firmware protocol 5 zachowuje istniejący bootloader, format
metadanych oraz położenie i rozmiar slotów A/B. Paczka update zawiera wyłącznie
obraz aplikacji przeznaczony dla nieaktywnego slotu. Aplikacja nie zapisuje
bootloadera.

Factory albo fizyczny dostęp będą potrzebne, jeżeli kiedyś zmienimy:

- kod samego bootloadera;
- layout flash lub format metadanych nieczytelny dla bootloadera protocol 2;
- elektryczny sposób wejścia do maintenance;
- obraz w sposób przekraczający rozmiar obecnego slotu.

Żadna z tych zmian nie jest potrzebna do framingu 3 i application protocol 5.

## 2. Sekwencja pierwszej migracji

1. Host protocol 4 wysyła do starej aplikacji adresowane polecenie wejścia do
   bootloadera.
2. Bootloader protocol 2 przyjmuje obraz protocol 5 do nieaktywnego slotu,
   sprawdza rozmiar i CRC, a następnie uzbraja trial boot.
3. Nowa aplikacja startuje z framingiem 3/application 5 i nie potwierdza trial
   automatycznie.
4. Host wykonuje Identity i Capabilities; aplikacja dopuszcza obsługę dopiero
   po lokalnym health check sensora, konfiguracji i RS485.
5. Host wysyła osobne, idempotentne `CONFIRM_TRIAL_BOOT`.
6. Aplikacja zapisuje potwierdzenie przez istniejące boot metadata API i zwraca
   stan po operacji.

Jeżeli handshake, health check albo confirm nie zakończą się w ograniczonym
czasie, aplikacja trial sama wywołuje restart. Przy kolejnym starcie bootloader
widzi niepotwierdzoną próbę i wraca do ostatniego aktywnego slotu. Sam brak
odpowiedzi hosta nie może pozostawić trial uruchomionego bezterminowo.

## 3. Kolejne aktualizacje

Protocol 5 musi zawierać operację Lifecycle `ENTER_UPDATE`, która używa
istniejącego `app_request_enter_bootloader()` i kontrolowanego restartu. Dzięki
temu po pierwszej migracji następne obrazy nadal można instalować po RS485.

`CONFIRM_TRIAL_BOOT` i `ENTER_UPDATE` są operacjami zmieniającymi stan. Po
timeout host ponawia ten sam zamiar z tym samym `operation_id`. Potwierdzenie
jest idempotentne także po zapisie metadanych i utracie odpowiedzi.

Implementacja trial daje hostowi 30 s od uruchomienia gotowego runtime.
Nieudana inicjalizacja lokalna powoduje natychmiastowy restart. Upływ deadline
bez confirm również restartuje MCU; istniejący bootloader rozpoznaje drugie
uruchomienie niepotwierdzonego trial i wybiera poprzedni aktywny slot.

## 4. Brama przed wdrożeniem bez USB

Przed aktualizacją jedynego zdalnego node test HIL na urządzeniu dostępnym
fizycznie musi potwierdzić:

- update protocol 2 do slotu przeciwnego;
- poprawny start protocol 5;
- confirm dopiero po handshake;
- wymuszony brak confirm, restart i automatyczny rollback;
- ponowne wejście do bootloadera z aplikacji protocol 5;
- ponowną aktualizację protocol 5 → kolejna wersja protocol 5.

Bez przejścia całej bramy obraz nie jest dopuszczony do zdalnego node.

Brama została wykonana na board rev 2 również dla aktualizacji protocol 5
`0.5.4` → `0.5.5`, bez użycia portu USB do komunikacji. Host dopuszcza starsze
capabilities schema 1 wyłącznie w komendzie przygotowania tej migracji i tylko
po zgodności hardware ID, board revision, node address, application protocol
oraz dokładnej wersji `0.5.4`. Zwykły handshake pozostaje rygorystyczny i wymaga
schema 2. Po aktualizacji host potwierdził trial `0.5.5` dopiero po pełnym
handshake schema 2.
