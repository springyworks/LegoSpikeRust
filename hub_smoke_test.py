from pybricks.hubs import PrimeHub
from pybricks.parameters import Color
from pybricks.tools import wait

hub = PrimeHub()
hub.display.char("H")
hub.light.on(Color.GREEN)
wait(1200)
hub.light.off()
print("Smoke test OK")
