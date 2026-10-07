// mDNS experiment, responder: join "Jumperly", announce mcu1.local and _lan._tcp:7000.
#include <WiFi.h>
#include <ESPmDNS.h>

void setup() {
  Serial.begin(115200);
  Serial.println("Ready");
  WiFi.mode(WIFI_STA);
  WiFi.begin("Jumperly", "");
  while (WiFi.status() != WL_CONNECTED) delay(100);
  Serial.printf("ip=%s\n", WiFi.localIP().toString().c_str());
  if (MDNS.begin("mcu1")) {
    MDNS.addService("lan", "tcp", 7000);
    Serial.println("mdns: responder mcu1.local");
  } else {
    Serial.println("mdns: begin failed");
  }
}

void loop() { delay(1000); }
