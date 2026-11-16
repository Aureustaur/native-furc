# ROADMAP.md

## Overview
This document outlines the parity milestones for reimplementing the Furcadia client on Linux. The milestones are derived from the provided steps and user answers. Open questions and risks are also included.

## Milestones

### Networking/Login
- **Milestone:** Implement basic networking functionality to connect to the Furcadia server.
- **Description:** Develop the necessary code to establish a connection to the Furcadia server using the provided network protocol.
- **Dependencies:** None.
- **Open Questions:** Protocol reverse-engineering.
- **Risks:** Network protocol may require reverse-engineering to understand the communication format.

### Pounce/Character Monitoring
- **Milestone:** Implement character monitoring and pounce functionality.
- **Description:** Develop the code to monitor character movements and detect pounce events.
- **Dependencies:** Networking/Login.
- **Open Questions:** Specific implementation details of pounce feature.
- **Risks:** Pounce feature may require additional reverse-engineering.

### Chat Rendering
- **Milestone:** Implement chat rendering functionality.
- **Description:** Develop the code to render chat messages in the client interface.
- **Dependencies:** Networking/Login.
- **Open Questions:** Chat message formatting and display requirements.
- **Risks:** Chat message formatting may vary across different clients.

### Avatar/Map Display
- **Milestone:** Implement avatar and map display functionality.
- **Description:** Develop the code to render avatars and maps in the client interface.
- **Dependencies:** Networking/Login.
- **Open Questions:** Specific requirements for avatar and map display.
- **Risks:** Display requirements may vary across different clients.

### DragonSpeak Support
- **Milestone:** Implement DragonSpeak support.
- **Description:** Develop the code to handle DragonSpeak commands and scripts.
- **Dependencies:** Networking/Login.
- **Open Questions:** Specific DragonSpeak commands and script structure.
- **Risks:** DragonSpeak commands and scripts may require reverse-engineering.

### Dream Editor Compatibility
- **Milestone:** Implement compatibility with the Dream Editor.
- **Description:** Develop the code to allow the client to open and edit Dream Editor files.
- **Dependencies:** DragonSpeak Support.
- **Open Questions:** Specific requirements for Dream Editor compatibility.
- **Risks:** Dream Editor compatibility may require reverse-engineering.

### Sound (FMOD Replacement)
- **Milestone:** Implement sound functionality using an FMOD replacement.
- **Description:** Develop the code to handle sound playback using an FMOD replacement library.
- **Dependencies:** Networking/Login.
- **Open Questions:** Specific requirements for sound playback.
- **Risks:** Sound playback may require reverse-engineering.

### Packaging for Linux
- **Milestone:** Package the client for Linux.
- **Description:** Develop the code to package the client for Linux distribution.
- **Dependencies:** All previous milestones.
- **Open Questions:** Specific packaging requirements for Linux.
- **Risks:** Packaging may require additional configuration or setup.

## Open Questions
- What is the specific implementation details of the pounce feature?
- What are the specific requirements for chat message formatting and display?
- What are the specific requirements for avatar and map display?
- What are the specific DragonSpeak commands and script structure?
- What are the specific requirements for Dream Editor compatibility?
- What are the specific requirements for sound playback?

## Risks
- Protocol reverse-engineering may be required to understand the communication format.
- Pounce feature may require additional reverse-engineering.
- Chat message formatting may vary across different clients.
- Display requirements may vary across different clients.
- DragonSpeak commands and scripts may require reverse-engineering.
- Dream Editor compatibility may require reverse-engineering.
- Sound playback may require reverse-engineering.
