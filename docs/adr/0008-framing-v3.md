# ADR-0008: framing protocol 3

- Status: accepted
- Date: 2026-09-29

## Kontekst

Application protocol 5 pracuje obecnie przez half-duplex RS485 115200 8N1,
często za konwerterem USB. Jedna linia może mieć wiele node, a host jest jedynym
inicjatorem. Parser nie może zależeć od przerw widzianych przez Linux, ufać
długości z uszkodzonego nagłówka ani alokować pamięci według danych z przewodu.

## Decyzja

1. Ramka wire to `COBS(raw_frame) || 0x00`.
2. Integralność całego nagłówka i payloadu chroni CRC-32C (Castagnoli).
3. `MAX_PAYLOAD` wynosi 512 B, a maksymalna ramka wire 530 B.
4. Nagłówek ma 10 B: version, role, destination, source, service, message oraz
   `transaction_id u32 LE`.
5. Nagłówek nie zawiera długości. Długość wynika z delimitera i zweryfikowanego
   rozmiaru odkodowanej ramki.
6. Na channel istnieje najwyżej jedna aktywna transakcja. Response powtarza
   transaction ID requestu.
7. Node odpowiada wyłącznie na adresowany request. Broadcast nie ma response.
8. Decoder używa stałych buforów. Po overflow odrzuca bajty do kolejnego `0x00`.
9. Framing nie wykonuje retry, schedulingu, commit, synchronizacji ani operacji
   aplikacyjnych.

Normatywny układ bajtów definiuje
[FRAMING-V3-SPEC.md](../protocol/FRAMING-V3-SPEC.md).

## Uzasadnienie

COBS daje jednoznaczną granicę niezależną od latencji USB i ograniczony narzut.
CRC-32C daje silniejszą ochronę długotrwałego strumienia niż CRC-16, a przy
11520 B/s koszt programowy RP2350 jest mały i zostanie zmierzony. Limit 512 B
mieści typowy chunk próbek, ogranicza RAM oraz najgorszy czas blokowania channel.

## Konsekwencje

- utrata, dodanie lub zmiana bajtu odrzuca ramkę; decoder odzyskuje granicę na
  delimiterze;
- framing wykrywa uszkodzenie, ale nie naprawia warstwy elektrycznej;
- 1000 XYZ/s przy 115200 pozostaje granicznym profilem bez wystarczającego
  marginesu retry; admission control ocenia go z pełnym kosztem transakcji;
- bootloader update protocol 2 pozostaje niezależny;
- zmiana któregokolwiek pola wire wymaga nowej wersji framingu.
