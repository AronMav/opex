---
name: smart-home
description: Smart home control — lights, switches, climate via Home Assistant
status: draft
triggers:
  - turn on the light
  - turn off
  - smart home
  - lamp
  - home temperature
  - home assistant
  - outlet
  - air conditioner
  - включи свет
  - выключи
  - умный дом
  - лампа
  - температура дома
  - розетка
  - кондиционер
priority: 8
tools_required:
  - ha_states
  - ha_turn_on
  - ha_turn_off
  - ha_call_service
state: active
---

## Smart Home Control Strategy

### Querying state
1. `ha_states` → get all devices
2. Filter by type: light.*, switch.*, climate.*, sensor.*, media_player.*
3. Show only active/relevant ones, grouped by room

### Control
- "Turn on living room light" → `ha_turn_on(entity_id="light.living_room")`
- "Set brightness to 50%" → `ha_call_service(domain="light", service="turn_on", entity_id="light.living_room", brightness=128)`
- "Set temperature to 22" → `ha_call_service(domain="climate", service="set_temperature", entity_id="climate.thermostat", temperature=22)`
- "Turn everything off" → `ha_turn_off` for each active device

### Safety Rules
- CONFIRM before mass operations ("turn everything off")
- Do not turn off security devices (camera.*, alarm.*) without explicit request
- Do not change climate by more than ±3°C at once — warn the user
- Do not control locks (lock.*) without double confirmation

### Status
Tools are in draft mode — to activate:
1. Configure Home Assistant URL (replace homeassistant.local with actual address)
2. Create Long-Lived Access Token in HA (Profile → Security → Create Token)
3. Add token to secrets: `POST /api/secrets` with name=HA_TOKEN
4. Verify tools via UI (Tools → Verify)
