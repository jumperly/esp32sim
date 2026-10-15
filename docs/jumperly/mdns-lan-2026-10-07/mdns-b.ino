// mDNS experiment, querier: join "Jumperly", wait 2 s, look mcu1.local up three ways.
#include <WiFi.h>
#include <ESPmDNS.h>

void setup() {
  Serial.begin(115200);
  Serial.println("Ready");
  WiFi.mode(WIFI_STA);
  WiFi.begin("Jumperly", "");
  while (WiFi.status() != WL_CONNECTED) delay(100);
  Serial.printf("ip=%s\n", WiFi.localIP().toString().c_str());
  if (!MDNS.begin("mcu2")) Serial.println("mdns: begin failed");
  delay(2000);
  IPAddress ip = MDNS.queryHost("mcu1", 3000);
  Serial.printf("mdns: queryHost mcu1 -> %s\n", ip.toString().c_str());
  int n = MDNS.queryService("lan", "tcp");
  Serial.printf("mdns: _lan._tcp %d\n", n);
  for (int i = 0; i < n; i++)
    Serial.printf("  %s %s:%d\n", MDNS.hostname(i).c_str(), MDNS.address(i).toString().c_str(), MDNS.port(i));
  IPAddress byName;
  if (WiFi.hostByName("mcu1.local", byName))
    Serial.printf("mdns: hostByName mcu1.local -> %s\n", byName.toString().c_str());
  else
    Serial.println("mdns: hostByName mcu1.local failed");
  Serial.println("done");
}

void loop() { delay(1000); }
