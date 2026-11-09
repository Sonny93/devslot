# Spec v1 : outil d'isolation des stacks de dev (révision 3)

Nov 9, 2026 · @Sonny

Révision du 9 novembre, après la revue de la [révision 2](https://claude.ai/artifact/7yfSVj6xwfQ4wfj2HuMMCd) : un bloc délimité dans `.env` remplace `.env.local`, `ds` écrase toujours ses propres variables, `ds rm` ramène un checkout à sa configuration de base, et il ne reste plus de question ouverte pour la v1.

## Problème et objectifs

L'outil permet de faire tourner plusieurs instances de la même stack sur une machine (plusieurs projets, ou plusieurs worktrees d'un projet) sans conflit de ports ni de projets compose.

Aujourd'hui, chaque checkout publie les mêmes ports (Adonis 3333, Postgres 5432, HMR Vite 24678) et partage le même nom de projet compose. Deux agents dans deux worktrees se marchent dessus : mêmes conteneurs, même volume, et `migration:fresh` efface la base de l'autre.

Objectifs de la v1 :

- Chaque checkout reçoit des ports stables, attribués automatiquement, et son propre projet compose.
- Chaque instance a ses propres services et ses propres données. Rien n'est partagé entre instances en v1.
- Une fois le checkout inscrit, les services et l'app démarrent avec les bonnes valeurs, qu'on passe par `ds` ou non.
- `ds rm` ramène le checkout à sa configuration de base : ports par défaut, rien d'écrit par l'outil.
- Les agents savent utiliser l'outil grâce à un plugin Claude Code livré avec lui.
- On peut lister toutes les instances de la machine, en texte et en JSON.

## Contraintes

1. **Un repo marche pareil avec ou sans l'outil.** Après un clone, ou après un `ds rm`, `just dev` fonctionne avec les ports par défaut.
2. **Une seule modification du projet, et elle est recommandée, pas imposée.** Pour qu'un port de service soit géré, le compose doit l'écrire `${VAR:-défaut}`. Un port en dur reste tel quel, avec un avertissement.
3. **L'app tourne hors Docker**, comme aujourd'hui.
4. **Une seule écriture dans le projet** : le bloc délimité de `.env`. Chaque écriture est annoncée dans la sortie, avec le chemin du fichier.
5. **Les intégrations sont optionnelles.** Tunneler, un proxy ou un gestionnaire de process sont proposés, jamais imposés.

Syntaxe conseillée dans les composes de dev : `127.0.0.1:${DB_PORT:-5432}:5432`. Avec la valeur par défaut, le compose marche toujours seul.

**Hors v1** : services partagés entre instances, process en arrière-plan, URLs nommées, intégration Tunneler, ports en dur remappés, Windows et macOS (le code reste portable, mais rien n'est testé dessus).

## Nom de l'outil

Nom retenu pour la v1 : **devslot**, alias court `ds`, pour « un emplacement de dev par checkout ». Il peut encore changer tant que rien n'est publié.

Alternatives libres sur crates.io : portlane (`pl`), berthing (`bt`), dockslot (`dk`). Déjà pris : berth, quay, pier, moor, slip, wharf, bollard. Vérifié le 9 novembre 2026 ; devslot semble aussi libre sur npm et GitHub.

## Concepts

- **Checkout** : un dossier qui contient une copie de travail du repo, que ce soit le clone principal ou un worktree. Trois worktrees plus le clone principal font quatre checkouts.
- **Instance** : un checkout inscrit dans le registre, identifié par son chemin canonique. C'est la racine git du dossier courant (`git rev-parse --show-toplevel`, distincte pour chaque worktree), ou le dossier courant hors git.
- **Nom d'instance** : `<repo>-<dossier>` plus 4 caractères d'un hash du chemin, par exemple `my-links-feat-auth-3f2a`. Le nom du repo vient du dossier de `git rev-parse --git-common-dir`, le même pour tous les worktrees. Le nom est mis en minuscules et réduit aux caractères que Compose accepte (`a-z`, `0-9`, `-`, `_`). Il sert de `COMPOSE_PROJECT_NAME`.
- **Slot** : un bloc de 50 ports consécutifs (taille réglable dans la config globale, jusqu'à 100). Le slot N couvre `20000 + 50×N` à `+49`. Une instance a un seul slot. Le slot 0 reste réservé aux serveurs partagés de la v2, pour que la v2 n'ait pas à déplacer les instances existantes.
- **Registre** : un fichier global à la machine qui garde, pour chaque instance, son chemin, son nom, son slot et le port attribué à chaque variable.

Le nom du projet compose dépend du chemin, pas du slot. Un slot libéré puis donné à un autre worktree ne récupère donc jamais les volumes de l'ancien.

## Commandes

Toutes les commandes agissent sur l'instance du dossier courant, sauf `ls` et `prune` qui couvrent la machine. Seuls `up` et `run` inscrivent un checkout, attribuent des ports et écrivent le bloc de `.env` ; les autres commandes ne modifient rien.

| Commande | Effet |
| --- | --- |
| `ds up` | Inscrit le checkout si besoin, écrit le bloc de `.env` et démarre les services du compose. Si le `devslot.toml` a une clé `run`, lance ensuite cette commande au premier plan. Ctrl+C arrête l'app ; les services continuent. |
| `ds run -- <cmd>` | Inscrit le checkout si besoin, écrit le bloc et lance n'importe quelle commande avec l'environnement de l'instance, sans démarrer le compose. Ex. : `ds run -- just dev`, `ds run -- node ace migration:run`. |
| `ds doctor` | Passe le projet en revue sans rien démarrer ni écrire, et liste les erreurs et avertissements décrits dans Vérifications. Marche aussi sur un checkout non inscrit. `--json` pour les agents. |
| `ds env` | Affiche l'environnement de l'instance. `--format sh` (pour `eval`) ou `json`. |
| `ds info` | Nom, chemin, ports et état de l'instance courante. `--json` pour les agents. Sur un checkout non inscrit, le dit et propose `ds up`. |
| `ds ls` | Toutes les instances de la machine. `--json` pour les scripts et Tunneler. |
| `ds down` | Arrête les services de l'instance, sans toucher aux données. |
| `ds reset [service]` | Remet à zéro les données de l'instance : supprime les volumes de ses services compose. |
| `ds rm` | Ramène le checkout à sa configuration de base : arrête les services, supprime ses conteneurs et ses volumes, retire le bloc de `.env` et libère son slot. Demande confirmation, sauf avec `--yes`. |
| `ds prune` | Retire les instances dont le dossier n'existe plus, et propose de supprimer leurs conteneurs et volumes. |

**Lancement de l'app.** `ds up` ne devine rien : il lance la commande `run` du `devslot.toml`, par exemple `node ace serve --hmr`. Sans clé `run`, `up` s'arrête après les services ; `ds run -- <cmd>` lance alors n'importe quel process, en Node, Python, Go ou autre.

**Pourquoi `run` ne lance que l'app.** C'est `ds` qui gère les services du compose. La clé `run` doit lancer l'app seule, pas une recette qui refait un `docker compose up`. En v1 ce serait sans danger, mais cette règle prépare le partage de la v2.

`ds up` ne lance pas le Justfile tout seul, parce qu'une recette peut déjà gérer le compose. Pour garder ta commande habituelle : `ds run -- just dev`, ou simplement `just dev` une fois le bloc écrit.

## Découverte des ports

L'outil trouve seul les variables de port, en lisant le compose et le `.env`, sans config.

1. **Fichier compose** : celui de la config, sinon le premier qui existe parmi `dev.compose.yaml`, `dev.compose.yml`, `compose.dev.yaml`, `compose.dev.yml`, `docker-compose.dev.yaml`, `docker-compose.dev.yml`, puis les noms que Docker prend par défaut (`compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`). Si le fichier retenu n'est pas l'un de ces quatre derniers, `ds` met aussi `COMPOSE_FILE` dans l'environnement et dans le bloc de `.env`, avec le `compose.override.*` du projet s'il existe. Un `docker compose` lancé sans `-f` vise ainsi le même fichier.
2. **Ports des services, par la méthode des sentinelles** :
   1. `docker compose config --variables` liste les variables du modèle et leurs valeurs par défaut.
   2. `docker compose config --format json` est lancé avec interpolation, en donnant à chaque variable candidate une valeur sentinelle unique (un port hors de la plage, différent pour chacune).
   3. Chaque champ `published` qui contient une sentinelle donne une variable gérée, avec son service et son port conteneur. C'est Docker qui fusionne les `include`, `extends` et fichiers multiples, et qui interprète `${VAR:-défaut}`. On évite `--no-interpolate`, connu pour mal rendre certaines syntaxes de ports.
3. **Ports en dur** (`"5432:5432"`) : un avertissement qui propose la syntaxe `${VAR:-5432}`. Ils ne sont pas gérés en v1.
4. **Port de l'app** : `PORT`, lu dans `.env`, sinon `.env.example`, plus les variables listées dans `[ports] extra`. Les autres `*_PORT` du `.env` ne sont pas touchées : elles peuvent viser un serveur externe (SMTP, base distante).
5. **HMR Vite** : `VITE_HMR_PORT` est ajoutée si `vite` est dans le `package.json`.
6. **Ce qui casserait l'isolation** : `ds up` refuse de démarrer si la config fusionnée contient un `container_name`, un volume ou un réseau avec un `name:` explicite ou `external: true`. Ces ressources seraient partagées entre instances malgré le projet compose distinct. Le message nomme le service et la clé en cause. Un bind mount en chemin absolu donne un avertissement. La liste complète est dans Vérifications.

Résultat sur ton template Adonis, pour une instance au slot 1 :

| Variable | Source | Par défaut | Attribué |
| --- | --- | --- | --- |
| `PORT` | `.env` | 3333 | 20050 |
| `VITE_HMR_PORT` | `package.json` (vite) | 24678 | 20051 |
| `DB_PORT` | compose, service `postgres` | 5432 | 20052 |

**Limite connue** : Vite n'a pas de variable d'environnement pour son port HMR. Il faut une ligne dans `vite.config.ts`, qui retombe sur le défaut sans l'outil : `server: { hmr: { port: Number(process.env.VITE_HMR_PORT) || undefined } }`.

## Vérifications

Les mêmes contrôles servent à `ds up` et à `ds doctor`. Une erreur bloque `ds up` ; un avertissement s'affiche sans bloquer. `ds up` ne contrôle que le compose qu'il lance, alors que `ds doctor` passe en revue tous les fichiers compose de la racine, celui de la prod compris.

| Vérification | Niveau | Pourquoi |
| --- | --- | --- |
| `container_name` sur un service | erreur | Deux instances voudraient le même nom de conteneur. |
| Volume ou réseau avec `name:` explicite ou `external: true` | erreur | Il serait partagé entre instances malgré le projet compose distinct. |
| `.env` absent | erreur | `ds` n'a pas où écrire son bloc. |
| Port publié en dur (`"5432:5432"`) | avertissement | Il n'est pas géré en v1 et entrera en conflit dès la deuxième instance. |
| Service qui charge `.env` avec `env_file` et reçoit une variable gérée sans la fixer dans `environment:` | avertissement | Le conteneur recevrait un port côté machine (voir la règle ci-dessous). |
| Bind mount en chemin absolu | avertissement | Les instances partageraient les mêmes fichiers. |
| `vite` dans le `package.json`, mais `VITE_HMR_PORT` absent de `vite.config.*` | avertissement | Toutes les instances se disputeraient le port HMR 24678. |
| `.env` non ignoré par git | avertissement | Le bloc finirait dans un commit. |
| `.env.test` qui redéfinit une variable gérée | avertissement | Les tests viseraient d'autres ports que l'app. |
| Valeur qui contient un port en dur (`DATABASE_URL=…:5432/…`) | avertissement | Elle ne suit pas le port attribué, sauf si `[env]` la reconstruit. |

**Règle pour `env_file`** : les valeurs du bloc sont des ports côté machine. Un conteneur qui charge `.env` doit donc fixer ses ports internes dans `environment:`, qui passe avant `env_file`. C'est le cas du service `app` du `compose.yml` de ton template : sans ces deux lignes, un `just prod` lancé dans un checkout qui utilise `ds` donne à l'app `PORT=20050` et `DB_PORT=20052`, et elle devient injoignable.

```yaml
  app:
    env_file:
      - .env
    environment:
      - DB_HOST=postgres
      - DB_PORT=5432
      - PORT=3333
```

`ds doctor --json` renvoie la même liste, avec pour chaque point le fichier, le service et la clé en cause. L'option `--fix`, qui corrigerait les fichiers après avoir montré un diff, viendra après la v1.

## Attribution et registre

Une instance garde les mêmes ports tant qu'elle existe. Ses favoris et ses tunnels ne cassent donc pas d'un redémarrage à l'autre.

1. **Verrou** : `flock` sur `registry.lock`. Deux agents lancés en même temps ne prennent jamais le même slot.
2. **Instance connue** : elle récupère ses ports. Une nouvelle variable prend le prochain port libre de son slot. Une variable qui a disparu garde son port, au cas où elle reviendrait.
3. **Slot plein** : une erreur claire, qui indique le nombre de ports demandés et propose d'augmenter la taille des slots dans la config globale. Pas de second slot automatique.
4. **Nouvelle instance** : elle prend, à partir de 1, le plus petit slot absent du registre dont tous les ports sont libres. Un port est libre si un `bind` réussit sur `127.0.0.1`, `0.0.0.0`, `::1` et `::`. Une erreur `EADDRNOTAVAIL` sur les adresses IPv6 est ignorée, pour les machines où IPv6 est désactivé.
5. **Vérification avant chaque `up`** : un port occupé par les conteneurs de l'instance est normal. Occupé par un autre programme, c'est une erreur qui nomme le port et, si possible, le programme.
6. **Écriture atomique** : fichier temporaire puis `rename`, avant de libérer le verrou.

Le registre est en TOML, lisible et modifiable à la main, dans `~/.local/state/devslot/registry.toml` :

```toml
version = 1

[[instance]]
path = "/home/sonny/dev/my-links.feat-auth"
name = "my-links-feat-auth-3f2a"
slot = 1
ports = { PORT = 20050, VITE_HMR_PORT = 20051, DB_PORT = 20052 }
```

La plage se règle dans `~/.config/devslot/config.toml`. Par défaut, 20000 à 29999 avec des slots de 50 ports, soit 200 slots. Cette plage reste sous celle des ports éphémères de Linux (32768 à 60999 par défaut).

## Environnement et bloc devslot dans .env

L'outil donne les valeurs de l'instance de deux façons : il les passe au process qu'il lance, et il les recopie dans un bloc délimité à la fin de `.env`. Avec ou sans `ds`, tout le monde lit donc les mêmes valeurs.

**Variables gérées** : chaque variable de port, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE` quand il le faut (voir Découverte des ports), les valeurs construites de `[env]`, plus `DEVSLOT_INSTANCE` et `DEVSLOT_SLOT` dans l'environnement des process lancés. Les identifiants de connexion, l'`APP_KEY` et le reste du `.env` ne sont jamais touchés.

**`ds` écrase toujours ses propres variables.** Si l'environnement de l'appelant en définit déjà une avec une autre valeur, par exemple après un `eval $(ds env)` fait dans un autre checkout, `ds` la remplace et le signale. Pour forcer une valeur, on passe par le `devslot.local.toml`.

**Pourquoi ça suffit** : Compose prend les variables du shell avant celles du `.env`, et `COMPOSE_PROJECT_NAME` passe avant le `name:` du fichier. Adonis donne la priorité à `process.env` sur ses fichiers `.env`.

| Fichier | Lu par Compose | Lu par Adonis en dev | Lu par Adonis en test |
| --- | --- | --- | --- |
| `.env` | oui | oui | oui |
| `.env.local` | non | oui | non |

C'est pour cette raison que le bloc va dans `.env` : `.env.local` ne serait lu ni par Compose ni par les tests, et créerait deux sources de vérité.

**Le bloc** ressemble à ceci :

```
# >>> devslot : généré automatiquement, ne pas modifier à la main >>>
# Pour revenir aux ports par défaut : ds rm
# (supprime aussi les conteneurs et les données de ce checkout)
COMPOSE_PROJECT_NAME=my-links-feat-auth-3f2a
PORT=20050
VITE_HMR_PORT=20051
DB_PORT=20052
# <<< devslot <<<
```

**Règles d'écriture** :

- Le bloc est écrit par `ds up` et `ds run`, jamais par les commandes de lecture. Seul le contenu entre les marqueurs change, par écriture atomique, et chaque écriture est annoncée avec le chemin du fichier.
- Si `.env` n'existe pas, `ds` s'arrête et demande de le créer d'abord, par exemple depuis `.env.example`.
- Un avertissement signale un `.env` non ignoré par git (`git check-ignore`), et un `.env.test` qui redéfinit une variable gérée.
- Si une variable gérée existe déjà plus haut dans le fichier (`PORT=3333`), le bloc à la fin suffit si Compose et Adonis gardent la dernière valeur d'une clé en double. Sinon, `ds` commente la ligne d'origine (`# PORT=3333  # devslot`) et la restaure à la sortie. À trancher au prototype.
- Un bloc supprimé à la main est réécrit au prochain appel à `ds up` ou `ds run`. La vraie sortie, c'est `ds rm`.
- `ds rm` retire le bloc et restaure les lignes commentées. Le checkout reprend alors les ports par défaut et le `name:` du compose.
- On peut couper l'écriture dans la config globale ou dans `[write]` du `devslot.toml`. Dans ce cas, seules les commandes lancées par `ds run` ont les valeurs de l'instance.

**Valeurs qui contiennent un port en dur**, comme `DATABASE_URL=postgres://…:5432/app` : un avertissement les signale. La section `[env]` de la config permet de les reconstruire à partir des ports attribués ; elles vont alors aussi dans le bloc.

## Plugin pour les agents

Le repo de l'outil contient un plugin Claude Code, partie intégrante de la v1. Grâce au bloc de `.env`, un agent qui oublie `ds` vise quand même les bons ports : le plugin sert surtout à lui donner les bonnes habitudes et ses ports dès le départ.

- **Un skill `devslot`.** Un skill ne se déclenche pas selon les fichiers du repo : c'est le modèle qui le choisit d'après sa description. Celle-ci doit donc nommer clairement le cas d'usage (un repo avec un compose, un bloc devslot dans `.env` ou un `devslot.toml`). Ses règles :
  - démarrer l'environnement avec `ds up` ;
  - lire les ports et les URLs avec `ds info --json`, sans jamais les deviner ni les écrire en dur ;
  - remettre ses données à zéro avec `ds reset` ;
  - ne jamais lancer `ds rm` ni supprimer des conteneurs ou des volumes à la main : d'autres agents travaillent sur la machine.
- **Un hook `SessionStart`, optionnel.** Si le checkout est inscrit, il ajoute la sortie de `ds info` au contexte de l'agent. Il n'inscrit rien et n'attribue rien.

Pas de hook `CLAUDE_ENV_FILE` ni `PreToolUse` en v1. Le bloc de `.env` couvre déjà les commandes lancées sans `ds`, et `COMPOSE_PROJECT_NAME` dans ce bloc limite un `docker compose down -v` à l'instance courante. Ces hooks dépendraient en plus de bugs connus de Claude Code (environnement perdu après un `/resume`, `CwdChanged` qui ne se déclenche pas avec `EnterWorktree`).

Pour les autres agents (Codex, Cursor…), le README contient les mêmes règles, à coller dans un `AGENTS.md`.

Installation : `/plugin marketplace add Sonny93/devslot`, puis `/plugin install devslot@<nom du marketplace>`. Les deux noms dépendront du nom définitif.

## Fichier de configuration

Un `devslot.toml` à la racine du repo, optionnel, commité ou ignoré par git au choix. Il règle la façon dont l'outil traite le projet, et peut déclarer la commande de l'app avec la clé `run`.

Un `devslot.local.toml`, ignoré par git, le surcharge pour ta machine. Ordre de priorité : la ligne de commande, `devslot.local.toml`, `devslot.toml`, la config globale, puis les valeurs par défaut.

```toml
# devslot.toml : tout est optionnel

# Commande de l'app, lancée par ds up au premier plan après les services.
# Elle lance l'app seule, jamais le compose : c'est ds qui gère les services.
run = "node ace serve --hmr"

[compose]
file = "dev.compose.yml"   # sinon détecté

[ports]
extra = ["FRONT_PORT"]     # variables à gérer en plus de PORT
ignore = ["MAILPIT_PORT"]  # variables du compose à laisser telles quelles

[env]
# valeurs construites à partir des ports attribués, écrites aussi dans le bloc
VITE_API_URL = "http://localhost:${PORT}"

[write]
enabled = true             # false : rien n'est écrit dans .env
```

La clé `run` est le seul process que le fichier peut déclarer : une seule commande, au premier plan. `ds info` affiche l'origine de chaque valeur : ligne de commande, fichier ou détection.

## Sortie JSON et Tunneler

En v1, `ds ls --json` est un contrat stable et versionné. C'est la seule chose dont Tunneler aura besoin, et l'outil n'a aucune dépendance vers Tunneler. La clé `shared` existe déjà, vide en v1, pour que la v2 n'ait pas à changer le contrat.

```json
{
  "version": 1,
  "shared": [],
  "instances": [
    {
      "name": "my-links-feat-auth-3f2a",
      "path": "/home/sonny/dev/my-links.feat-auth",
      "slot": 1,
      "services_running": true,
      "ports": [
        { "var": "PORT", "port": 20050, "kind": "web" },
        { "var": "VITE_HMR_PORT", "port": 20051, "kind": "web" },
        { "var": "DB_PORT", "port": 20052, "kind": "service", "service": "postgres", "target": 5432 }
      ]
    }
  ]
}
```

L'intégration Tunneler viendra en v2, sous deux formes, toutes deux optionnelles :

- **Suggestion** : `ds tunnels --host nova` affiche la commande à lancer sur ton PC principal, `tunneler up nova -L 20050 -L 20051 -L 20052`. Dans `-L`, un port seul désigne le même port sur la machine distante, donc les numéros sont identiques des deux côtés.
- **Côté Tunneler** : il lance `ds ls --json` sur l'hôte, par sa session SSH, et propose les tunnels qui manquent. Les instances étant prévisibles, des tunnels en standby sur les premiers slots ne coûtent rien tant que personne ne s'y connecte.

## Stack technique

Un seul binaire Rust, qui reprend les conventions de Tunneler : édition 2024, clippy pedantic, pas d'`unwrap` hors tests, CI Linux, binaires publiés dans les releases GitHub.

| Besoin | Choix |
| --- | --- |
| CLI | `clap` (derive) |
| Lecture du compose | `docker compose config --variables`, puis `config --format json` avec des valeurs sentinelles, lu avec `serde_json`. Pas de parseur YAML. |
| `.env` | `dotenvy` pour la lecture ; écriture du bloc maison, entre marqueurs, par fichier temporaire et `rename` |
| Registre et config | `serde` + `toml` |
| Verrou | `fs4` (`flock`) |
| Lancement de l'app | `exec` sous Unix : la commande remplace l'outil, donc Ctrl+C et le code de sortie se comportent comme sans lui. Sous Windows, plus tard : lancer, attendre, relayer le code. |
| Docker | Appels à la commande `docker compose`, sans SDK |

Côté tests, des tests unitaires pour la découverte, l'attribution et l'écriture du bloc, et des tests d'intégration avec un faux `docker` (un script désigné par `DEVSLOT_DOCKER`) pour ne pas dépendre d'un vrai démon dans la CI.

## À vérifier au prototype

Ce ne sont plus des questions de conception : un essai sur le template Adonis suffit à y répondre.

- [ ] La méthode des sentinelles retrouve bien chaque variable de port, avec son service et son port conteneur.
- [ ] Compose et Adonis gardent la dernière valeur d'une clé en double dans `.env`. Sinon, commenter la ligne d'origine.
- [ ] La ligne de `vite.config.ts` suffit pour isoler le HMR.
- [ ] Aucun de tes composes n'a de `container_name`, ni de volume ou réseau avec `name:` ou `external: true`.
- [ ] Deux worktrees, `ds up` dans chacun, puis un `migration:fresh` dans l'un : la base de l'autre reste intacte.
- [ ] Un `eval $(ds env)` dans A, un `cd` dans B, puis `ds run` : les ports sont ceux de B.
- [ ] Un `just dev` lancé sans `ds` après un premier `ds up` : le compose et l'app visent les mêmes ports.
- [ ] Un agent Claude Code dans chaque worktree, qui travaillent en même temps.
- [ ] `ds rm` ramène bien le checkout sur 3333, 5432 et 24678.

**Limite connue, documentée sans être traitée en v1** : les cookies ne sont pas isolés par port. Deux instances ouvertes sur `localhost` avec le même nom de cookie de session se déconnectent l'une l'autre dans le navigateur. Les URLs nommées de la v2 règlent ce cas, puisque chaque instance aura son propre nom d'hôte.

## Après la v1

Ce qui est prévu pour la v2, chaque brique étant optionnelle et construite sur le noyau de la v1.

### Services partagés

Un service du compose lancé depuis une image publique (Postgres, MySQL, Redis…) est remplacé par un serveur partagé que l'outil gère lui-même, sur un port du slot 0, avec une base isolée par instance. Le partage se fait par image exacte, avec un petit adaptateur par famille pour créer la base et le rôle de chaque instance.

**Le piège à éviter.** Si `DB_PORT` vaut le port du serveur partagé et qu'un `docker compose up` est lancé, Compose démarre aussi le Postgres du compose. Celui-ci essaie de publier le même port, déjà pris, et le démarrage échoue.

**La parade : un override hors du repo.**

- `ds` génère un fichier d'override dans `~/.local/state/devslot/<instance>/override.yml`. Il donne au service remplacé un profile que personne n'active : Compose ne le démarre plus.
- **Règle : `DB_PORT` et `COMPOSE_FILE` sont toujours écrits ensemble**, dans l'environnement comme dans le bloc de `.env`. Compose lit `COMPOSE_FILE` depuis `.env`, donc la règle tient aussi pour un `just dev` lancé sans `ds`. Après un `ds rm`, rien ne reste : Compose lance son Postgres sur le port par défaut, sans conflit.
- Quand `COMPOSE_FILE` est défini, Compose ne charge plus tout seul le `compose.override.yml` du projet : `ds` doit l'ajouter à la liste s'il existe.
- `depends_on` : la doc de Compose confirme qu'une dépendance vers un service désactivé par profile renvoie une erreur. L'override devra donc aussi retirer ce `depends_on` des services qui en dépendent, par exemple avec `!reset`. À valider au prototype de la v2.

Reste à reprendre de la [révision 1](https://claude.ai/artifact/TxgSxb3jrF9tM1kwQRGCGn) : les adaptateurs par famille, la règle Redis (partagé seulement si l'app lit `REDIS_DB`), le partage des services `build:`, la persistance (`persist`) et l'option `--isolated`.

### Autres briques

- **Clonage de base** : copier la base d'une instance vers une autre. Pour Postgres, `CREATE DATABASE … TEMPLATE` duplique la base côté serveur.
- **Process en arrière-plan** : `ds up -d`, avec les logs et un arrêt global. Soit un superviseur intégré, soit PM2 en dessous.
- **URLs nommées** : un proxy qui route `feat-auth.my-links.localhost` vers le port web de l'instance, ce qui isole aussi les cookies. Une intégration optionnelle de portless (Vercel Labs) est à comparer avec un proxy maison.
- **Tunneler** : `ds tunnels`, puis la lecture de `ds ls --json` par Tunneler.
- **Ports en dur dans le compose** : le même mécanisme d'override, pour les remapper.
- **`ds doctor --fix`** : corriger les points détectés par `ds doctor`, après avoir montré le diff et demandé confirmation.
- **`ds rm --keep-data`**, si le besoin apparaît : revenir aux ports par défaut sans supprimer les volumes.
- **Prod locale, à étudier** : devslot reste un outil de dev. Un support de la prod lancée en local (l'app dans Docker) ne viendra que si le besoin se confirme. En attendant, la règle `env_file` de Vérifications suffit pour qu'un `just prod` marche dans un checkout qui utilise `ds`.

## Décisions

Plus aucune question ouverte pour la v1.

**Révision 3 (9 novembre)** :

- Un bloc délimité à la fin de `.env` remplace `.env.local` et `.env.test.local`. Il contient les ports, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE` quand il le faut, et un commentaire qui donne `ds rm`.
- `ds` écrase toujours ses propres variables, avec un avertissement.
- `ds rm` retire le bloc, supprime les conteneurs et les données du checkout et libère son slot. Un bloc retiré à la main est réécrit au prochain `ds up` ou `ds run`.
- Seuls `up` et `run` inscrivent un checkout et écrivent ; `info`, `env` et `doctor` ne modifient rien.
- `ds doctor` en v1, en lecture seule, avec les mêmes contrôles que `ds up` sur tous les fichiers compose de la racine. `--fix` après la v1.
- Règle `env_file` : un conteneur qui charge `.env` fixe ses ports internes dans `environment:`. Les valeurs du bloc sont des ports côté machine.
- devslot reste un outil de dev ; la prod locale est à étudier plus tard.
- La découverte des ports passe par la méthode des sentinelles, plus par `--no-interpolate`.
- Ports de l'app limités à `PORT`, `VITE_HMR_PORT` et `[ports] extra`.
- `ds up` refuse de démarrer si un `container_name`, ou un volume ou réseau nommé explicitement, casserait l'isolation.
- Un seul slot par instance : un slot plein donne une erreur claire, et la taille des slots se règle dans la config globale.
- Test de port libre aussi en IPv6 (`::1` et `::`).
- Pas de hook `CLAUDE_ENV_FILE` ni `PreToolUse` en v1.
- Retirés : `ds nuke`, `ds release`, `ds skill`, le format `dotenv` de `ds env`, le second slot automatique, `ds init` (`docker init` fait déjà ce travail).

**Révision 2, toujours valables** : pas de partage de services en v1 ; clé `run` dans `devslot.toml` ; partage en v2 avec `DB_PORT` et `COMPOSE_FILE` toujours écrits ensemble ; le nom devslot ; des slots de 50 ports ; des fichiers de config en TOML.
