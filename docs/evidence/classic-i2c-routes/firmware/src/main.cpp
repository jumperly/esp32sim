#include <Arduino.h>
#include <Wire.h>

void probe(TwoWire &wire, int port, int sda, int scl) {
  wire.end();
  wire.begin(sda, scl, 100000);
  wire.beginTransmission(0x29);
  wire.write(0x77);
  int ack = wire.endTransmission();
  int count = wire.requestFrom(uint8_t(0x29), uint8_t(1));
  int value = count ? wire.read() : -1;
  Serial.printf("ROUTE:%d:%d:%d:%d:%d\n", port, sda, scl, ack, value);
}
void setup() {
  Serial.begin(115200);
  for (int port = 0; port < 2; ++port) {
    TwoWire &wire = port ? Wire1 : Wire;
    probe(wire, port, 21, 22);
    probe(wire, port, 18, 19);
    probe(wire, port, 21, 19);
  }
  Serial.println("ROUTE:done");
}
void loop() { delay(1000); }
