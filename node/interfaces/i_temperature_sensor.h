#pragma once

#include "sensor_types.h"

/*
============================================================
ITemperatureSensor
============================================================

Abstrakcyjny interfejs dla urządzeń, które potrafią podać
informację o temperaturze.

Może być implementowany przez:
- akcelerometr z wewnętrznym czujnikiem temperatury
- osobny sensor temperatury
- inne urządzenie wielofunkcyjne
============================================================
*/

class ITemperatureSensor {
public:
    virtual ~ITemperatureSensor() = default;

    virtual SensorStatus read_temperature(TemperatureSample& temperature) = 0;
};