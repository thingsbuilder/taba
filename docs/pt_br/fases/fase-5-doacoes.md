# Fase 5 — Doações

> Leia antes: [README das fases](README.md) e a [spec](../spec/spec.md). Este documento recorta a spec; se houver contradição, vale a spec e o dono decide.

## Objetivo
Entregar doações pelo cartão do núcleo familiar, com duplicidade sinalizada (nunca bloqueada), liveness só em itens de alto valor e num sorteio de 5%, e aprovação do admin para quem não faz ou não pode fazer selfie.

**Entrega demonstrável em produção:** o admin cadastra uma cesta (`per_household`, 7 dias), um kit de higiene (`per_person`), um colchão (`once_per_event`, `high_value`); o voluntário lê o cartão de um núcleo e entrega; a segunda cesta ao mesmo núcleo vai para revisão; o colchão pede selfie; para uma pessoa sem biometria, o admin aprova na hora; uma entrega offline de colchão sem admin no aparelho vai para revisão.

## Escopo
**Itens (3.5.2)**
- `PUT /v1/shelters/{id}/items`: por item, `scope` (`per_person` | `per_household`), `period_days` (padrão 7) ou `once_per_event`, `high_value`.

**Entrega (3.5.3, 4.7)**
- Leitura do QR (ou digitação do código), escolha de quem retira entre os membros do núcleo (cache offline).
- `POST /v1/shelters/{id}/deliveries` com UUID do cliente.
- Liveness pedida (online e fora do modo degradado) para `high_value` e para o sorteio: `sha256(delivery_id)` como número em [0, 1) menor que `DELIVERY_SAMPLE_RATE` (0,05); resposta `428 liveness_required`; captura `purpose = revalidation` com `expected_person_id`; `person_id` diferente → `409 revalidation_failed`, nada gravado.
- Pessoa que não faz ou não pode fazer selfie (recusa, `no_biometrics`, sem rosto): **aprovação do admin** — admin do abrigo, com a própria sessão (inclusive pela troca de operador por PIN), reenvia com `admin_approval: true`; servidor exige admin (`403 admin_approval_required`) e grava `verification = admin_approval`, `approved_by`, `status = ok` e auditoria com o nome do admin. Não vai para revisão.
- Pessoa `assisted` recebe pelo cartão como qualquer outra; a selfie é oferecida, nunca exigida.
- Duplicidade em todos os eventos ativos: `per_person` por `(person_id, item)`, `per_household` por `(household_id, item)`, no período ou no evento (`once_per_event`) → `status = in_review`, `duplicate_in_period`, com a entrega anterior. **Nunca bloqueia.** A resposta não mostra o abrigo da outra entrega a abrigo não confirmado.
- Offline ou em modo degradado: cartão pelo cache, sem sorteio; `high_value` usa a aprovação do admin se houver sessão de admin no aparelho; senão, `in_review` com `liveness_unavailable`.
- Revisão pelo admin: `GET .../deliveries?status=in_review` e `POST /v1/deliveries/{id}/review` (`reviewed_ok` | `irregular`, com nota).
- A biometria tardia (4.12) passa a reapontar `deliveries.person_id`.

**PWA**
- Tela `/shelter/:id/delivery` (leitura de QR, membros, item, quantidade, fluxo de liveness e de aprovação do admin), configuração de itens e lista de revisão para o admin. Navegação inferior com "Doações" (design/README.md).

## Fora de escopo
Pseudonimização e descarte das entregas (fase 6); exportação CSV (fase 6).

## Seções da spec
2.3 (aprovar entrega, revisar, configurar itens), 3.5.2, 3.5.3, 3.8 (revalidação na entrega), 4.1, 4.7, 4.12, 5.7 (entrega em modo degradado), 6.2.2, 7 (`shelters.items`, `deliveries`), 8.6, 9.1, 9.2, 10.1.1 (`fraud_prevention`), 10.4.

## Critérios de aceite da seção 15
- **4** — em modo degradado, check-in sem biometria, núcleo, cartão e entrega comum funcionam; `high_value` usa a aprovação do admin ou vai para `in_review` com `liveness_unavailable`.
- **25** — recusa gera `no_biometrics` com `biometrics_declined`; a pessoa entra em núcleo, recebe cartão e recebe entrega comum (`201`, `verification = card`).
- **27** — pessoa `assisted` recebe entrega comum sem `428`.
- **28** — entrega comum não sorteada → `201`, `verification = card`, sem chamada à Unico.
- **29** — `high_value` sem `capture_id` → `428`; com captura de outra pessoa → `409` e nada gravado; com `admin_approval` por admin → `201`, `ok`, `admin_approval`, `approved_by` e auditoria; por voluntário → `403 admin_approval_required`.
- **30** — mesma entrega reenviada tem o mesmo resultado de sorteio.
- **31** — `per_household` para dois membros do mesmo núcleo no período → o segundo `in_review`; `per_person` não.
- **32** — `once_per_event` de novo no mesmo evento, mesmo depois de `period_days` → `in_review`.
- **33** — offline, `high_value` sem admin no aparelho → `in_review` com `liveness_unavailable`; com aprovação do admin → `ok`.
- **54** — offline: criar abrigo, 3 check-ins por foto, 1 sem biometria, 1 núcleo com cartão, 2 entregas, 1 resgate e 1 busca; ao religar, tudo sincroniza em ordem, `Blob`s apagados, contador zerado.

## O que a fase cria
| Tipo | Nomes |
|---|---|
| Coleções e campos | `deliveries`; `shelters.items` |
| Rotas | `PUT /v1/shelters/{id}/items`, `POST /v1/shelters/{id}/deliveries`, `GET /v1/shelters/{id}/deliveries?status=`, `POST /v1/deliveries/{id}/review` |
| Erros | `liveness_required`, `admin_approval_required`, `card_not_found`, `person_not_in_household` |
| Variáveis | `DELIVERY_SAMPLE_RATE` |
| Módulo | `deliveries.rs` |
| PWA | `/shelter/:id/delivery`, configuração de itens, revisão |
| Índices | `deliveries (person_id, item, delivered_at)`, `deliveries (household_id, item, delivered_at)`, `deliveries (shelter_id, status, delivered_at)` |

## Ajustes desde que o doc foi escrito (fase 5, execução noturna)
- **PIN desligado (decisão do dono, fase 2):** a "troca de operador por PIN" só alcança quem já tinha PIN. A aprovação do admin no mesmo aparelho usa a sessão de um admin que esteja no aparelho; sem ela, o app oferece "Entrar como admin com selfie" (login normal) e, se não for possível, a entrega de alto valor segue as regras offline (`in_review`, `liveness_unavailable`).
- **Navegação inferior:** o app ainda não tem barra inferior; "Doações" entra como botão na tela do abrigo (e no cartão lido), e a barra inferior fica como follow-up.
- **Criação do abrigo e núcleos:** a entrega reusa o cartão e o núcleo da fase 3 (inclusive "Só esta pessoa"); a tela de leitura de cartão já existe em `Household.tsx`.

## Dependências
Fase 3 (núcleos, cartões, níveis, revalidação, PIN de admin no aparelho, modo degradado, fila) e fase 4 (resgate e busca, usados no critério 54).

## Testes
**Unitários (obrigatórios):** sorteio determinístico (mesmo `delivery_id` → mesmo resultado; taxa aproximada em amostra grande); decisão de pedir liveness (alto valor, sorteio, offline, degradado); aprovação do admin (admin, voluntário, offline com e sem admin no aparelho); duplicidade `per_person`, `per_household`, `once_per_event`, limites do período e eventos ativos; `assisted` e `no_biometrics` recebendo; revisão; reapontamento de entregas na biometria tardia; não vazar o abrigo da outra entrega para abrigo não confirmado.

**Manuais com a Unico:** entrega de item `high_value` com a selfie certa e com outra pessoa (2); esperado 2 a 4 processos.

**Manuais sem a Unico:** cenário offline completo do critério 54 no iPhone com o app instalado; aprovação do admin por troca de operador no mesmo aparelho.

## Riscos e pontos de atenção
- A fila de distribuição não pode travar: entrega comum nunca chama a Unico.
- A aprovação do admin é um ponto de abuso possível; a auditoria com o nome do admin é obrigatória.
- Duplicidade entre eventos ativos exige os índices certos; confira as cotas de leitura do Firestore (6.3.5).

## Checklist de pronto
- [ ] Itens com escopo, período e alto valor.
- [ ] Entrega por cartão online e offline, com sorteio, liveness e aprovação do admin.
- [ ] Duplicidade sinalizada e revisão pelo admin.
- [ ] Biometria tardia reapontando entregas.
- [ ] Índices aplicados.
- [ ] Critérios 4, 25, 27–33 e 54 verificados; 61 e 62 conferidos de novo.
- [ ] Testes manuais com a Unico feitos e contados no PR.
- [ ] QA aprovado, CI verde, PR mergeado pelo dono, deploy verificado em produção.

## Regras de processo (resumo; detalhe no README)
Senior staff engineer implementa; QA compara com a spec e com este doc item a item. Uma branch e um PR; o dono revisa; depois do merge, deploy em `taba.thingsbuilder.com`. Testes unitários em tudo; testes automáticos nunca chamam a Unico. Código, commits e PRs em inglês. Nada da instalação no repo. Problemas na documentação da Unico: registrar no arquivo interno de problemas da documentação da Unico. Pronto = QA aprovado + CI verde + PR mergeado + deploy verificado.
