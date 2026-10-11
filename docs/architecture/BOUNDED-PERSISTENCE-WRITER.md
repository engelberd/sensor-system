# Bounded persistence writer

- Status: worker core, handoff `NodeSession`/`MeasurementCoordinator` oraz
  wątek production executor zaimplementowane; batch pojedynczego WAL jest
  gotowy, grouped worker i jego ograniczony adapter wątkowy obsługują
  completion/recovery i są włączone do composition; fault injection bariery
  gotowy, pozostaje benchmark polityki
- Ostatnia aktualizacja: 2026-10-08

## Cel

Pętla kanału RS485 nie może wykonywać blokującego `fsync`. Po odebraniu i
zweryfikowaniu odpowiedzi przekazuje należący do niej rekord do ograniczonej
kolejki storage. Node otrzymuje durable/release dopiero po completion workera.

```text
ChannelAgent -> bounded submit -> storage executor -> CaptureRepository
     ^                                  |
     +------- DurableReceipt -----------+
```

## Zaimplementowany rdzeń

`PersistenceEnvelope` posiada pełny rekord i wcześniej wyliczony
`DurableReceipt`. Konstruktor waliduje chunk oraz koniec zakresu przed
enqueue. Dzięki temu sukces storage nie może zostać unieważniony późniejszym
błędem lokalnego wyliczenia cursora.

`BoundedPersistenceWorker` posiada:

- stałą, konfigurowalną pojemność;
- rosnący `WriteJobId` do korelacji completion;
- kolejkę rekordów należących do workera;
- osobną, również ograniczoną kolejkę wyników;
- jawny backpressure przy braku miejsca;
- fail-closed po pierwszym błędzie sinka;
- zachowanie zadania o niepewnym wyniku oraz możliwość wstawienia ponownie
  otwartego/recovered sinka.

Do limitu liczą się jednocześnie rekordy oczekujące oraz nieodebrane completion.
Dlatego zatrzymany kanał, storage albo coordinator nie może spowodować
nieograniczonego wzrostu RAM. Przy błędzie powstaje dodatkowo jeden mały
completion diagnostyczny, ale rekord nadal zajmuje to samo, wcześniej
zarezerwowane miejsce.

Worker zatrzymuje się na pierwszym błędzie storage. Nie konsumuje późniejszych
zadań, ponieważ kolejność cursorów musi zostać zachowana. Zadanie o niepewnym
wyniku pozostaje własnością workera. Po recovery sinka jest automatycznie
wstawiane przed późniejsze zadania i otrzymuje ten sam `WriteJobId`. Failure
completion informuje o awarii nośnika; dopiero późniejszy success completion
może przesunąć durable cursor sesji.

## Zależność od nośnika

Poprawność nie zależy od SD ani M.2 SSD. Nośnik wpływa na profil wykonawczy:

- pojemność kolejki;
- maksymalny czas oczekiwania na durable completion;
- liczbę rekordów w jednym group commit;
- próg ostrzeżeń latency i zajętości kolejki.

Docelowy Raspberry Pi z M.2 SSD zostanie użyty do benchmarku tych wartości.
Do czasu pomiaru nie zamrażamy arbitralnego batch size ani opóźnienia.

## Granica group commit

`CaptureWalBatch` przygotowuje rekordy bez zapisu, pilnuje ich kolejności i
zatwierdza je jedną barierą `sync_data`. `CaptureSegmentStream` rozpoczyna batch
dopiero po potrzebnej rotacji, a `CaptureRepositoryBatch` wiąże go z jednym
sensorem i boot epoch. Batch kończy się na rekordzie pieczętującym okno i nigdy
nie obejmuje kilku fizycznych plików.

Executor nadal używa batcha jednoelementowego. Nie wolno tylko zebrać kilku
zadań i wysłać success completion po wspólnym `sync_data`: jego błąd pozostawia
niepewną całą grupę. Dlatego grouped worker zachowuje wszystkie `WriteJobId`,
po ponownym otwarciu klasyfikuje każdy zakres względem recovered cursor i
dopiero wtedy potwierdza albo ponawia poszczególne rekordy. Dzięki temu
optymalizacja nie osłabia istniejącej ochrony przed duplikatem.

`classify_group_recovery` zamraża regułę tego rozstrzygnięcia. Akceptuje brak
trwałego rekordu, pełny ciągły prefiks albo całą grupę. Odrzuca pustą lub
nieciągłą grupę, inną tożsamość, niepoprawny zakres i cursor wewnątrz rekordu.
Pozostała praca dotyczy osadzenia tej semantyki na wątku i polityki oczekiwania,
nie formatu danych na dysku.

`GroupedPersistenceWorker` posiada już całą niepewną grupę, emituje błąd dla
jej pierwszego `WriteJobId`, a po recovery wystawia sukces dla potwierdzonego
prefiksu i ponawia wyłącznie suffix. Grupuje tylko rekordy tego samego
`hardware_id`, boot epoch i dokładnie przyległych cursorów. Limit grupy i cała
kolejka są ograniczone. Test z rzeczywistym `CaptureRepository` prowadzi trzy
rekordy przez jeden repository batch.

`GroupedPersistenceExecutor` osadza ten rdzeń na nazwanym wątku. Jeden wspólny
licznik atomowy ogranicza rekordy znajdujące się łącznie w kanale, workerze i
niepewnej grupie. `GroupCommitPolicy` ustala `max_records` oraz `max_wait`;
wartości pozostają parametrami composition. Recovery ma osobny request/response:
wywołujący otrzymuje wynik klasyfikacji, więc błędny evidence nie może zniknąć
w tle. Test obejmuje pełną grupę, backpressure, częściowo odzyskany prefiks i
ponowienie suffixu na wątku.

Testowy failpoint WAL działa po zapisaniu całego batcha, lecz przed uznaniem
durability barrier za udaną. Test filesystem wymusza ten błąd dla trzech
rekordów, ponownie otwiera repozytorium, odzyskuje cursor końca grupy i wymaga
trzech success completion bez drugiego appendu. Failpoint nie jest obecny w
buildzie produkcyjnym.

## Zaimplementowany handoff

`NodeSession` przenosi pełny rekord do `PersistenceEnvelope` bez kopiowania
wektora próbek i zachowuje tylko oczekiwany `DurableReceipt`.
`MeasurementCoordinator` parkuje dany node, zwalnia fizyczny kanał dla innych
node i przechowuje wiele zadań przez `WriteJobId`. Błąd storage nie przesuwa
cursora. Po recovery wynik jest klasyfikowany z durable cursora: brak rekordu
powoduje retry, a rekord już trwały daje completion bez duplikowania appendu.

## Executor produkcyjny

`PersistenceExecutor` posiada sink na osobnym nazwanym wątku. Producer używa
`try_send` do ograniczonego kanału, więc pętla RS485 nigdy nie czeka na I/O.
Failure completion zatrzymuje worker na tym samym rekordzie. Po przekazaniu
recovered sinka worker zachowuje ten sam `WriteJobId` i wykonuje jawne
`RetryUncommitted` albo `ConfirmAlreadyDurable`. Shutdown opróżnia kanał
completion i zwraca sink oraz wszystkie wyniki, których coordinator jeszcze
nie odebrał.

`ThreadedChannelRecordingLoop` jest pierwszym pionem używającym tej ścieżki.
Symulacja czterech node potwierdza rotację kanału w czasie, gdy poprzednie
chunki należą już do storage. Osobny test filesystem prowadzi rzeczywisty
`CaptureRepository` przez WAL, `sync_data`, completion i durable cursor na
wątku executora.

## Pozostała integracja

1. Podłączyć profile instalacji do gotowego `CaptureRuntimeConfig`, recovered
   snapshotu i kontrolowanego shutdownu.
2. Ustalić w `HostCoordinator`, czy 1–16 kanałów współdzieli executor, czy używa
   małej, mierzonej puli; decyzja zależy od realnego latency nośnika.
3. Dobrać wartości polityki benchmarkiem przed zamrożeniem profilu
   produkcyjnego; rozszerzyć testy procesu o kill podczas rotacji i publikacji.

Ta integracja nie może zmienić zasady: `DurableReceipt` pojawia się wyłącznie po
barierze obejmującej konkretny rekord.
