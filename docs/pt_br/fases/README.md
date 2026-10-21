# Fases de implementação do Taba

Este diretório divide a implementação do piloto em sete fases. Cada fase é iniciada numa sessão nova, sem o histórico das conversas de especificação. Por isso cada documento de fase é **autossuficiente**: diz o que construir, onde está o detalhe na especificação e como saber que acabou.

- Especificação: [`docs/pt_br/spec/spec.md`](../spec/spec.md). É a fonte de verdade. O doc da fase **recorta** a spec; nunca a contradiz. Se encontrar contradição, pare e pergunte ao dono.
- Infraestrutura: [`infra/README.md`](../../../infra/README.md) e `infra/ansible/`.
- Design system: [`design/README.md`](../../../design/README.md), `design/tokens.css` e `design/tokens.json`.

## Ordem

| Fase | Documento | Entrega demonstrável em produção |
|---|---|---|
| 0 | [Fundação](fase-0-fundacao.md) | PWA instalável em `taba.thingsbuilder.com` e `GET /v1/status` respondendo pela API, publicados pelo pipeline (CI → release → Ansible). |
| 1 | [Identidade do voluntário](fase-1-identidade.md) | Voluntário entra com Human Token (jornada com liveness), cria perfil, troca de operador por PIN; papéis globais por CLI. |
| 2 | [Abrigo e rede](fase-2-abrigo-e-rede.md) | Criar abrigo com GPS e endereço, evento automático, convidar voluntários, confirmar abrigo. |
| 3 | [Check-in](fase-3-check-in.md) | **Primeiro marco útil:** cadastro de pessoas com e sem selfie, núcleo familiar e cartão, criança, offline, modo degradado, saída. |
| 4 | [Resgate, busca e mediação](fase-4-resgate-busca-mediacao.md) | Registro no ponto de desembarque, busca por foto, filas de mediação, encerramento de evento. |
| 5 | [Doações](fase-5-doacoes.md) | Entrega pelo cartão, duplicidade sinalizada, liveness por alto valor e sorteio, aprovação do admin. |
| 6 | [Painel e LGPD](fase-6-painel-e-lgpd.md) | **Pronto para o piloto:** painel da Defesa Civil, CSV, descarte, anomalias, RIPD. |
| — | Depois do piloto | Seção 16 da spec ("Estou Seguro", aval remoto, fusões, fotos do local). Não faz parte destas fases. |

Resgate, busca e mediação (fase 4) vêm **antes** de doações (fase 5), por decisão do dono.

## Regras de processo (valem para todas as fases)

1. **Papéis.** Cada fase é implementada por um senior staff engineer e validada por um analista de QA. O QA compara a implementação com a spec (`docs/pt_br/spec/spec.md`) e com o doc da fase, **item a item**, e registra o resultado no PR.
2. **Branch e PR.** Uma branch e um PR por fase. O dono revisa o PR. Depois do merge, a fase sobe para produção (`taba.thingsbuilder.com`) e é testada lá.
3. **Testes unitários em tudo:** regras de negócio, validação, cálculos de distância, duplicidade, níveis, permissões, cifra, i18n etc.
4. **A Unico nunca é chamada por teste automático.** Não há sandbox e cada chamada é cobrada. Testes automáticos usam um servidor falso (`UNICO_URL`). Testes com a Unico real são **manuais**, listados no doc de cada fase, com a quantidade esperada de chamadas.
5. **Idioma.** Código, identificadores, mensagens de log, commits e PRs em inglês. Documentação em português.
6. **Nada da instalação no repositório:** ID de projeto GCP, IP e chaves ficam em `infra/ansible/inventory/prod/` (fora do git) e no vault.
7. **Problemas na documentação da Unico:** todo problema novo encontrado na documentação do Human Token deve ser registrado no arquivo interno de problemas da documentação da Unico.
8. **Definição de pronto de qualquer fase:** QA aprovado, testes unitários verdes no CI, PR mergeado e deploy verificado em produção.
9. **Critérios de outras fases continuam valendo.** Uma fase não pode quebrar critério já cumprido; o CI roda todos os testes. Os critérios 61 e 62 (segredos fora do repo e tudo em inglês) são conferidos pelo QA em toda fase, embora pertençam à fase 0.

## Mapeamento critério de aceite → fase

Critérios da seção 15 da spec. Cada critério pertence a exatamente uma fase: a fase em que ele pode ser verificado por inteiro pela primeira vez.

| Fase | Critérios |
|---|---|
| 0 Fundação | 61, 62 |
| 1 Identidade | 1, 6, 7, 57 |
| 2 Abrigo e rede | 8, 9, 10, 12, 14, 15, 17, 18, 19, 20, 21 |
| 3 Check-in | 2, 3, 5, 23, 24, 26, 34, 35, 36, 37, 38, 39, 40, 42, 43, 44, 45, 55, 56, 58 |
| 4 Resgate, busca e mediação | 11, 22, 46, 47, 48, 49, 50, 51 |
| 5 Doações | 4, 25, 27, 28, 29, 30, 31, 32, 33, 54 |
| 6 Painel e LGPD | 13, 16, 41, 52, 53, 59, 60 |

Total: 62 critérios, todos atribuídos. Os critérios do "Estou Seguro" (seção 16.1, item 10) não são numerados e ficam para depois do piloto.

Critérios que dependem de mais de uma fase foram atribuídos à última parte necessária:
- 4 (modo degradado com entrega) → fase 5; a fase 3 já entrega o modo degradado do check-in.
- 11 (encerramento de evento com aviso de busca) → fase 4, que cria a tarefa horária.
- 16 (restrições do abrigo não confirmado, incluindo exportação) → fase 6.
- 25 (sem biometria recebe entrega) → fase 5; a fase 3 entrega o cadastro sem biometria.
- 37 (saída sem destino → `422`) → fase 3; a contagem no painel é conferida no critério 52 (fase 6).
- 54 (offline completo, com entregas e busca) → fase 5.

## Como iniciar uma fase

1. Leia o doc da fase inteiro, depois as seções da spec que ele lista.
2. Leia o código das fases anteriores que a fase usa.
3. Crie a branch `phase-N-<name>` (em inglês).
4. Implemente com testes; rode o CI.
5. Abra o PR com: o que foi feito, a lista dos critérios cumpridos com o teste que cobre cada um, os testes manuais com a Unico (quantas chamadas foram feitas) e as mudanças de infra.
6. QA valida, o dono revisa e faz o merge, e a fase é publicada e verificada em produção.
