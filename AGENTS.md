# AGENTS.md — Nixblend

## Objetivo do projeto

Nixblend é um gerenciador de pacotes para NixOS com experiência de uso semelhante à de um gerenciador tradicional de distribuição. O projeto deve crescer de forma incremental, mantendo desempenho, previsibilidade e segurança na edição das configurações do NixOS.

## Princípios de arquitetura

- Use Rust estável e mantenha a implementação simples, modular e extensível.
- Comece como um único binário organizado por módulos. Não introduza uma arquitetura distribuída, plugins ou abstrações genéricas sem uma necessidade concreta.
- Separe responsabilidades:
  - `domain`: modelos e regras de negócio, sem depender de CLI, arquivos, processos ou ferramentas Nix.
  - `application`: um módulo por caso de uso e `ports.rs` com as interfaces (traits) de que precisam; `application::error` tem os tipos de erro.
  - `infrastructure`: implementações das interfaces: ferramentas Nix, edição de arquivos `.nix`, gravação atômica, processos, sinais, configuração e privilégios.
  - `cli`: definição dos comandos e argumentos, apresentação, perguntas, mensagens e diff.
  - `main.rs`: único ponto que conecta as camadas. As dependências apontam para dentro: `cli` → `application` → `domain`, e `infrastructure` implementa os ports.
- Mantenha a lógica de negócio independente da interface de terminal sempre que isso não gerar complexidade desnecessária.
- Defina interfaces pequenas entre módulos. Evite módulos que conheçam detalhes internos de todos os outros.
- Não antecipe funcionalidades futuras sem requisito real; preserve pontos de extensão claros.

## Padrões de código Rust

- Prefira código idiomático, explícito, legível e fácil de testar.
- Use `clap` para CLI e `thiserror` para erros próprios. Use `anyhow` apenas na camada de aplicação, quando fizer sentido.
- Evite `unwrap()` e `expect()` em caminhos normais de execução. Retorne erros com contexto útil.
- Não use `unsafe` sem necessidade técnica demonstrável e justificativa documentada.
- Evite alocações, clonagens, dependências e processos externos desnecessários. Não use async sem uma necessidade concreta.
- Não crie abstrações, traits ou camadas apenas por antecipação. Os ports de `application` já existem para isolar o exterior; novos só com necessidade concreta.
- Documente APIs públicas e decisões não óbvias; não escreva comentários que apenas repetem o código.
- Execute `cargo fmt` e `cargo clippy --all-targets --all-features -- -D warnings` antes de considerar uma alteração concluída.

## Segurança e manipulação de arquivos

- Nunca monte comandos de shell concatenando strings. Use `std::process::Command` com argumentos separados.
- Trate caminhos e entradas do usuário como dados não confiáveis; valide-os e apresente erros claros.
- Não sobrescreva `packages.nix` se a estrutura encontrada não for reconhecida ou se a alteração não puder ser aplicada com segurança.
- Preserve o conteúdo existente, a formatação e as entradas não relacionadas sempre que possível.
- Mostre um diff legível antes de qualquer gravação e peça confirmação explícita.
- Grave alterações de forma atômica: escreva um arquivo temporário no mesmo diretório, valide-o e então substitua o original. Evite deixar o arquivo parcialmente escrito.
- Não execute rebuild nem altere a configuração sem consentimento explícito do usuário.
- Não exponha senhas, tokens ou dados sensíveis em logs.

## Usuário e permissões

- O comando pode ser iniciado com `sudo`, mas os dados do Nixblend devem pertencer ao usuário que iniciou o comando, não automaticamente ao `root`.
- Ao executar sob `sudo`, identifique o usuário original de forma segura (por exemplo, `SUDO_USER` validado contra a base local de usuários) e resolva seu diretório home. Não confie cegamente em `HOME` ou em caminhos fornecidos por variáveis de ambiente quando o processo estiver elevado.
- Armazene a configuração do Nixblend em `$XDG_DATA_HOME/nixblend/config.toml`, usando `~/.local/share/nixblend/config.toml` como fallback padrão. Garanta permissões adequadas e não crie arquivos pertencentes a `root` no home do usuário.
- Diferencie edição do arquivo e privilégio para rebuild. Solicite elevação apenas para a operação que realmente precisar dela. Se o processo já for root, não invoque `sudo` novamente.
- Nunca tente capturar, armazenar ou solicitar a senha de root diretamente. Deixe o `sudo` solicitar a senha pelo terminal.

## Integração com NixOS

- Não presuma que todos os usuários organizam `packages.nix` da mesma forma. Detecte e suporte explicitamente os formatos definidos pelo projeto.
- Se a estrutura do arquivo for desconhecida, ambígua ou não suportada, pare sem alterar nada e explique o motivo.
- Valide a expressão Nix modificada antes de gravá-la, quando as ferramentas Nix necessárias estiverem disponíveis.
- Execute processos externos sem shell intermediário e confira o código de saída.
- Não considere a inclusão do pacote concluída apenas porque o texto foi gravado: informe separadamente o resultado da edição e o resultado do rebuild.

## Desempenho

- Priorize inicialização rápida e baixo consumo de memória.
- Evite dependências pesadas quando a biblioteca padrão ou uma dependência pequena resolver o problema.
- Não invoque `nix`, `nixos-rebuild` ou outros processos externos para tarefas que podem ser resolvidas localmente.
- Não faça varreduras recursivas ou leituras repetidas do mesmo arquivo sem necessidade.
- Mantenha o fluxo síncrono enquanto não houver uma necessidade concreta de concorrência. Threads existem apenas para ler pipes de processos e tratar sinais.

## Testes e qualidade

- Cubra com testes unitários a lógica de configuração, detecção do formato de `packages.nix`, geração do diff e inclusão idempotente de pacotes.
- Teste caminhos de sucesso e falha: arquivo ausente, formato não suportado, pacote duplicado, erro de permissão, falha de validação Nix e rebuild cancelado ou malsucedido.
- Use diretórios temporários nos testes; nunca altere a configuração real do sistema durante testes.
- Isole execução de comandos externos atrás de funções pequenas para que possam ser simuladas nos testes.
- Não marque uma tarefa como concluída sem executar os testes relevantes e informar qualquer verificação que não pôde ser feita.

## Regras para alterações feitas por agentes

1. Leia a estrutura existente antes de editar ou criar arquivos.
2. Faça a menor alteração coerente com o requisito.
3. Preserve o comportamento existente, salvo quando o requisito pedir explicitamente uma mudança.
4. Não substitua arquivos inteiros quando uma modificação localizada for suficiente.
5. Atualize testes e documentação afetados.
6. Ao finalizar, resuma arquivos alterados, comportamento implementado, testes executados e limitações conhecidas.
