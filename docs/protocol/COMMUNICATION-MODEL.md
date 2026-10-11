# Model komunikacji channel — dane, czas i polecenia bez kolizji

- Status: obowiązujący model zachowania; parametry schedulera do pomiaru
- Date: 2026-09-29
- Zakres: jeden half-duplex channel z jednym hostem i wieloma node

## 1. Cel

Channel ma możliwie szybko opróżniać `MeasurementJournal` każdego node, jednocześnie
wykonując krótkie pomiary synchronizacji czasu i dozwolone operacje sterujące.
Żadna klasa ruchu nie może nadawać samodzielnie ani wejść w środek innej ramki.

```text
Host request -> dokładnie jeden adresowany node response -> bus idle
Host request -> następny adresowany node response         -> bus idle
```

Na jednym RS485 channel istnieje zawsze najwyżej jedna aktywna transakcja. Inne
channels hosta działają równolegle i mają własne schedulery.

## 2. Trzy rodzaje kolizji

### Kolizja elektryczna

Dwa urządzenia próbują sterować parą RS485 jednocześnie. Zapobiegamy jej przez:

- host jako jedyny inicjator;
- node odpowiada wyłącznie na request skierowany do jego adresu;
- broadcast nigdy nie generuje odpowiedzi;
- dokładnie jeden request oczekujący na odpowiedź;
- jawny turnaround i powrót nadajnika do RX;
- timeout przed przejściem do następnej transakcji.

### Kolizja logiczna

Poprawna odpowiedź zostaje przypisana do niewłaściwego polecenia albo stara
odpowiedź wpływa na nową operację. Zapobiegamy jej przez:

- `source` i `destination`;
- `service` i `message`;
- 32-bitowy `transaction_id` powtarzany w odpowiedzi;
- sprawdzenie aktualnego mode i stanu sesji;
- idempotentne retry tego samego logicznego requestu.

### Kolizja polityk

Konfiguracja, diagnostyka lub sync zabiera pasmo próbkom albo zmienia znaczenie
danych w niejawnej chwili. Zapobiegamy jej przez scheduler i trzy mode.

## 3. Ramki mają zmienny rozmiar

`MAX_PAYLOAD = 512 B` oznacza górny limit, nie stały rozmiar każdej ramki.

Przykładowo przy proponowanym COBS i 10-bajtowym headerze:

| Ruch | Przykładowy payload | Przybliżona ramka wire | Czas przy 115200 8N1 |
| --- | ---: | ---: | ---: |
| Pusty request | 0 B | około 16 B | 1.4 ms |
| Time-sync response: dwa `u64` | 16 B | około 32 B | 2.8 ms |
| Pull 32 próbek | 26 B | około 42 B | 3.6 ms |
| 32 próbki XYZ i24 + 40 B timing | 363 B | około 380 B | 33.0 ms |
| Maksymalny payload | 512 B | około 530 B | 46.0 ms |

Sync nie jest dopełniany do rozmiaru danych. Nie przerywa rozpoczętej ramki;
scheduler umieszcza go pomiędzy transakcjami danych. Maksymalny rozmiar ramki
wyznacza najgorszy czas oczekiwania krótkiej operacji na wolną magistralę.
Podane rozmiary uwzględniają 64-bitowy `boot_epoch`; dokładny timing payload
pozostaje jeszcze otwarty.

## 4. Typy ruchu

### Transfer próbek

Host pyta konkretny node o zakres od znanego cursora i podaje limit liczby
próbek/bajtów. Node odpowiada jednym bounded chunk. Host waliduje sequence i
zapisuje dane trwale. Największy ciągły durable cursor może przekazać w następnym
request jako zgodę na zwolnienie starszych kopii. Osobna transakcja release jest
potrzebna przy kończeniu sesji lub dłuższej przerwie w transferze.

Host nigdy nie przesuwa cursora release przez niewyjaśnioną lukę. Szczegółowy
audyt starego `GrantBurstRead` i `CommitReadUpTo` znajduje się w
[LEGACY-TRANSACTION-AUDIT.md](LEGACY-TRANSACTION-AUDIT.md).

Większy chunk zmniejsza procentowy narzut, ale wydłuża zajęcie magistrali. Jego
rozmiar jest polityką schedulera i może być mniejszy niż `MAX_PAYLOAD`.

### Synchronizacja czasu

Host rozpoczyna krótki exchange. Minimalny wariant wykorzystuje:

```text
t1 = host monotonic przy wysłaniu requestu
t2 = node monotonic przy odbiorze requestu
t3 = node monotonic przy wysłaniu response
t4 = host monotonic przy odbiorze response
```

`transaction_id` koreluje exchange. Host zachowuje `t1` i `t4`, więc payload
response może zawierać tylko `t2` i `t3` jako dwa `u64`. Dokładna semantyka
momentu timestampu powstanie w etapie time sync.

Każdy node ma osobny deadline sync. Host rozkłada exchange w czasie; nie odpytuje
wszystkich node jednocześnie co pełną sekundę.

### Health

Kompaktowy health jest krótki i rzadki. Może być osobnym response albo później
zostać dołączony do odpowiedzi transferu, jeśli pomiar wykaże korzyść. Pełne
liczniki debug należą do `Diagnostic`, nie do obowiązkowej pętli Production.

### Polecenia

W `Production` scheduler przyjmuje tylko polecenia bezpieczne dla trwającego
pomiaru. Zmiana ODR, zakresu lub filtrów jest odrzucana albo wykonywana jako
transakcja na jawnej granicy `sample_seq`. Restart, commissioning i update
wymagają `Maintenance`.

## 5. Scheduler oparty o deadline, nie jedną kolejkę FIFO

Sam stały priorytet mógłby zagłodzić time sync, a zwykła rotacja mogłaby dopuścić
overflow najbardziej obciążonego node. Scheduler bierze pod uwagę:

- czas do przepełnienia `MeasurementJournal`;
- backlog i ODR node;
- deadline następnego time sync;
- retry deadline po błędzie;
- limit health/diagnostics;
- maksymalny czas zajęcia magistrali przez pojedynczy chunk.

Orientacyjna kolejność decyzji:

1. recovery danych zagrożonych overflow;
2. praca, której deadline zakończenia właśnie nadchodzi — według najbliższego
   deadline; obejmuje control, transfer, time sync i health;
3. normalny transfer według `time_to_overflow`, z maksymalnym deadline obsługi
   chroniącym wolniejsze node przed zagłodzeniem;
4. gotowe polecenie control;
5. diagnostyka tylko w profilu Diagnostic i z pozostałego budżetu.

Deadline dotyczy przewidywanego zakończenia transakcji, a nie dopiero chwili jej
rozpoczęcia. Kandydat staje się pilny, gdy `now + estimated_slot >= deadline`.
Każdy slot ma osobny górny limit; błędnie oszacowana praca przekraczająca limit
jest odrzucana zamiast niejawnie blokować channel.

Time sync ma zarezerwowany mały budżet i deadline, więc ciągły napływ próbek nie
może odsuwać go bez końca. Jednocześnie sync wielu node jest rozłożony w czasie.

Runtime estymuje `time_to_overflow` konserwatywnie: do ostatniego
zaobserwowanego `produced` dodaje próbki, które mogły powstać od tej obserwacji
przy skonfigurowanym output ODR. Obliczenie jest całkowitoliczbowe i zaokrągla
produkcję w górę. Wartość zero jest legalna i oznacza natychmiastowe ryzyko
utraty; nie jest zastępowana dodatnim sztucznym opóźnieniem.

## 6. Przykładowy przebieg

```text
HOST -> node A : PULL_DATA transaction 101
node A -> HOST : DATA 32 samples transaction 101

HOST -> node B : TIME_SYNC transaction 102
node B -> HOST : TIME_SYNC {t2, t3} transaction 102

HOST -> node C : PULL_DATA transaction 103
node C -> HOST : DATA 16 samples transaction 103

HOST -> node A : PULL_DATA {start, release_through} transaction 104
node A -> HOST : DATA transaction 104

HOST -> node B : PULL_DATA transaction 105
...
```

Każda para kończy się przed rozpoczęciem następnej. Sync i jawny release są
krótsze od typowej odpowiedzi danych, więc ich koszt jest mały i policzalny.

## 7. Zachowanie przy błędzie

Wspólne reguły deadline, guard time, nowych ID dla kolejnych prób oraz
uzgadniania niejednoznacznego wyniku definiuje
[TRANSACTION-POLICY.md](TRANSACTION-POLICY.md).

- brak odpowiedzi: timeout, zwolnienie magistrali, bounded retry;
- zły COBS/CRC: ramka odrzucona, brak skutku aplikacyjnego, retry;
- odpowiedź z innym transaction ID: odrzucona jako spóźniona/obca;
- odpowiedź innego node: odrzucona i raportowana;
- timeout nieznanego wyniku release/config: uzgodnienie stanu, nie ślepe wykonanie
  nowej operacji;
- rosnący backlog: ograniczenie diagnostyki, mniejsza częstotliwość health i
  priorytet node najbliższego overflow;
- konfiguracja przekracza trwałą przepustowość: odmowa startu przez admission
  control, zamiast późniejszej utraty danych.

## 8. Co trzeba zmierzyć

Przed zamrożeniem algorytmu schedulera mierzymy:

- rzeczywisty request/response turnaround każdego adaptera;
- rozkład czasu transferu ramek, nie tylko średnią;
- koszt COBS i CRC na RP2350;
- retry i CRC error rate na 400 m i 800 m;
- potrzebną częstotliwość sync dla zakładanej niepewności czasu;
- backlog i czas do overflow dla 250, 125 i 62.5 Hz;
- sprawiedliwość wielu node i czas reakcji krótkiego polecenia.

Na tej podstawie wybieramy domyślną liczbę próbek w chunku, sync interval,
turnaround, timeout oraz rezerwę pasma. Mechanizm pozostaje ten sam przy innych
wartościach.
