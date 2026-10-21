# Taba

[Português](README.pt-BR.md) · English

**Taba** is an open source network of shelters for disasters in Brazil, such as floods. When people lose their homes, they often lose their documents too. Taba recognizes people by their face, without documents, so shelters can work together as one network:

- know how many **unique** people are sheltered, not how many records exist;
- avoid duplicate registrations and duplicate donation pickups across shelters;
- help **families find missing relatives**, including people who cannot say their own name.

> **Status:** specification phase. Nothing is implemented yet. See the [specification](docs/en/spec/).

## How it works

Taba uses [Unico Human Token](https://developer.unico.io/human-token/index.html). From a selfie, Human Token returns a stable, opaque identifier: the same person always gets the same token. The token **does not say who the person is**, only that they are the same person as before.

Main features:

1. **Shelter check-in** by selfie, with no duplicates across shelters.
2. **Donations** with duplicate detection. A repeat pickup is flagged for review, never refused on the spot.
3. **Rescue registration** at the landing or triage point.
4. **Volunteers**, invited by QR code or link, always verified with Human Token.
5. **Missing person search** from photos sent by the family.
6. **"I'm safe"**: residents register themselves. Anyone who needs rescue is told to call 193 (Fire Department) or 199 (Civil Defense).
7. **Revalidation** for high-value donations and for releasing a child from a shelter.

Shelters can be created by a single operator with a phone. The app works offline and syncs when there is signal. A new shelter starts as **unconfirmed** and only sees its own data until it is confirmed. Shelters form **events** automatically, based on distance, with no back office needed to declare that a disaster has started.

**Stack:** installable web app (PWA) in TypeScript, API in Rust, Firestore, a single GCP e2-micro VM provisioned with Ansible ([infra/README.md](infra/README.md), in Portuguese).

## Legal considerations

Taba handles **biometric data of vulnerable people**. Anyone running an instance must take this seriously.

- **Biometric data is sensitive personal data** under Brazil's LGPD (art. 5, II). Each operation has a legal basis under art. 11. For example: protection of life for the missing person search, fraud prevention for donations, public policy for government operations. Details are in [base legal LGPD](docs/pt_br/base%20legal%20LGPD.md) (Portuguese).
- **Only the token is stored, never the photo.** The token itself is stored as an HMAC, so a leaked database cannot be matched against other systems. The one exception is a small, encrypted face thumbnail of a person with no identification, such as an unconscious person or a small child. It is deleted as soon as the person has a name or a wristband number.
- **The search never reveals a location to whoever searched.** A mediator (a shelter operator or Civil Defense), or the person who was found, decides whether contact happens. This protects victims from abusers looking for them.
- **Data is deleted when the event ends**, after a retention period.
- **Children** follow LGPD art. 14 (best interest of the child).
- **Data location.** The reference instance runs in the United States, which is an international data transfer under LGPD art. 33. Each installation can choose its own region. A city government can run its own instance in São Paulo.
- **Controller.** Whoever runs an instance in a real disaster, such as a city or a humanitarian organization, is the data controller. That organization needs its own data protection impact assessment (RIPD) and its own legal review.
- **Unico.** Each installation needs its own Unico access key and must follow Unico's terms of use.

This is not legal advice. Validate with a lawyer or data protection officer before using Taba with real data.

## Where the name comes from

*Taba* is a word from **Tupi**, an Indigenous language of Brazil. It means **village**: the group of *ocas* (houses) where a community lives together. In the same way, a disaster event in Taba is the group of shelters that work as one network.

Like many Brazilian words of Tupi origin, *taba* is part of everyday Portuguese. We use it with respect for the Indigenous peoples whose language gave it to us.

## Contributing

- Code, identifiers, API routes, commit messages and pull requests are in **English**.
- Documentation is in Portuguese ([docs/pt_br](docs/pt_br/)), with an English version of the specification ([docs/en](docs/en/)).
- Never commit secrets or real data. Each installation keeps its values in `infra/ansible/inventory/prod/` and in `.env`, both ignored by git.

## License

[GNU Affero General Public License v3.0](LICENSE). If you run a modified version of Taba as a service, you must publish your changes.
