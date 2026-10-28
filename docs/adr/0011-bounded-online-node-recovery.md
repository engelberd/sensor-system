# ADR-0011: bounded online recovery zamiast trwałej kwarantanny node

- Status: zaakceptowany
- Data: 2026-10-28

## Kontekst

Host wykrywa zmianę `boot_epoch`, nieprawidłowy release, błędną odpowiedź i
regres cursorów fail-closed. Pierwsza implementacja ustawiała wtedy
`recovery_required` i całkowicie usuwała node z rotacji. Chroniło to tożsamość
danych, ale bez automatycznej drogi powrotnej pojedyncze zakłócenie mogło
zatrzymać następne okna pomiarowe na wiele godzin.

Dla tego produktu utrzymanie kolejnych poprawnych okien jest ważniejsze niż
bezterminowa próba uratowania kilku niejednoznacznych próbek. Utrata nadal nie
może być cicha.

## Decyzja

`RecoveryRequired` jest przyczyną rozpoczęcia bounded recovery, a nie stanem
terminalnym. Dostępność node ma jawne stany `Online`, `RetryBackoff`,
`Recovering` i `Offline`.

Recovery jest pracą klasy `Control` wybieraną przez ten sam `ChannelScheduler`
co Measurement, TimeSync i Telemetry. Nie powstaje drugi arbiter RS485.
Measurement zagrożony przepełnieniem zachowuje pierwszeństwo. TimeSync i
Telemetry chorego node są wyłączone do odzyskania.

Jedna próba recovery wykonuje przez jedynego `ChannelAgent`:

1. Identity i Capabilities;
2. GetState;
3. GetStreamState;
4. uzgodnienie z durable Capture dla tej samej epoki albo trwały rollover
   Capture/Time/Telemetry dla nowej epoki;
5. doprowadzenie node do Recording i ponowne dołączenie do rotacji.

Pierwszy timeout kończy daną próbę, zamiast zajmować magistralę do pełnego
deadline procedury. Po wyczerpaniu krótkiego budżetu node przechodzi do
`Offline`, ale zachowuje okresowy, fazowany probe. Odpowiedź ponownie uruchamia
pełne recovery. Zdrowe node na tej samej linii pozostają obsługiwane.

Jeżeli prefiks nie jest już dostępny, host zapisuje dokładny `Gap` albo
`ContinuityBreak` i kontynuuje. Nie wolno zatrzymać przyszłych okien tylko po
to, aby bezterminowo chronić niejednoznaczny fragment historii.

## Konsekwencje

- `NodeSession` nadal tylko waliduje strumień i cursory.
- `ChannelRuntime` posiada politykę dostępności i terminy prób.
- worker wykonuje handshake, rollover i lifecycle, ale nie ustala własnego
  backoffu.
- status operatorski publikuje stan dostępności, liczbę prób i dokładny powód
  recovery.
- Maintenance i shutdown pomijają niedostępny strumień podczas drain zamiast
  faultować cały worker.

Brama MVP wymaga symulacji oraz HIL: timeout, błędna ramka, `InvalidRelease`,
regres cursora, restart/new epoch, odłączenie i późniejszy powrót node oraz brak
zagłodzenia zdrowego node.
