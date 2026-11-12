# ADR-0016: Neutralne wersyjnie ścieżki źródeł

- Data: 2026-11-12
- Status: zaakceptowane na żądanie użytkownika

## Kontekst

Po przełączeniu produkcji nazwy `host_v2/` i `node/protocol_v3/` przestały
oznaczać równoległe implementacje. Pierwsza była jedynym aktywnym recorderem,
a druga mieściła jednocześnie framing 3 i application protocol 6. Numery
generacji w ścieżkach utrudniały rozpoznanie źródła prawdy.

## Decyzja

- `host/` jest jedynym katalogiem produktu hosta;
- runtime Rust, Analysis, kontrakty i instalator leżą w `host/runtime/`;
- bieżący endpoint firmware leży w `node/protocol/`;
- wersje framing, application, schema danych i usług pozostają jawne w
  kontraktach, stałych, metadanych i nazwach dokumentów;
- nazwy zewnętrznych unitów `*-v2`, socketów i schema statusu pozostają
  tymczasowo bez zmian, ponieważ są interfejsem wdrożonych instalacji;
- historyczny Python recorder zostanie wydzielony z aktywnego `host/` w
  osobnym etapie, po sklasyfikowaniu zależności dashboardu i testów.

## Skutki

ADR-0004 pozostaje historią migracji, ale jego równoległy układ katalogów nie
opisuje już bieżącego repozytorium. Nowe skrypty nie mogą obliczać root repo na
podstawie dawnej głębokości `host_v2`; testy muszą sprawdzać wykonanie z
kanonicznych ścieżek.
