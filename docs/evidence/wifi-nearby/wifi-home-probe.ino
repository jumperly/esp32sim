// Joins the network "Home" with a password. A new board serves "Jumperly", so the simulator tells
// the user which network the sketch looks for; with the board's wifiSsid "Home" and wifiPassword
// "home-pass-123" it connects.
#include <WiFi.h>

void setup() {
  Serial.begin(115200);
  Serial.println("Ready");
  WiFi.begin("Home", "home-pass-123");
  unsigned long t0 = millis();
  while (WiFi.status() != WL_CONNECTED && millis() - t0 < 20000) delay(100);
  if (WiFi.status() == WL_CONNECTED)
    Serial.printf("connected ip=%s\n", WiFi.localIP().toString().c_str());
  else
    Serial.printf("not connected: status=%d\n", (int)WiFi.status());
}

void loop() { delay(1000); }
