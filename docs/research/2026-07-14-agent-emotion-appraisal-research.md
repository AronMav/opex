# Agent Emotion / Appraisal Layer — Research (2026-07-14)

Deep-research (109 агентов, 26 источников → 122 claims → 25 верифицировано → 23 подтверждено / 2 refuted; все выжившие — первичные peer-reviewed, единогласные 3-0). Цель: инженерная основа для «внутренней жизни» (appraisal-эмоции) — последнее незакрытое свойство души OPEX. Результат = основа спеки.

## Вывод

Appraisal-theory вычислительная эмоция — зрелый, инженерно-реализуемый субстрат. Первоисточники сходятся на прямо-реализуемом дизайне.

## Подтверждённые находки

1. **Эмоция appraisal'ится из ВНУТРЕННЕЙ модели агента (beliefs/desires/intentions/plans/goals), НЕ из поверхностного контента события** (EMA, Gratch & Marsella). Единая декларативная «causal interpretation», над которой быстрые доменно-независимые feature-detector'ы выводят appraisal-переменные; переоценка каждой цели непрерывно. → Для OPEX: аппрайзить события против существующего `session_goals`/`agent_plans` субстрата, не отдельным free-form проходом.

2. **Appraisal = малый вычислимый набор переменных** (три независимые первичные линии сходятся): desirability/conduciveness, likelihood/outcome-probability, agency/blame (intention+foreknowledge+coercion), novelty/suddenness/unpredictability, controllability, changeability, temporal-status. Диапазоны: Marinier/Laird/Lewis — ~11 численных ([0,1] и [-1,1]) + Causal Agent {self/other/nature}, Causal Motive {intentional/negligence/chance}. Sander-LLM: Ve/Vl/Vd/Va/Vco. Кросс-уок к вопросу 1:1.

3. **Каждое событие → типизированная intensity-эмоция по OCC** (22 метки как конъюнкции переменных). Intensity: OCC зависит от desirability×likelihood; Marinier даёт явную [0,1] функцию (surprise-factor × среднее остальных измерений), СОЗНАТЕЛЬНО отвергая чистое умножение (один ноль → занулит всё) + realization-принцип (неожиданное > ожидаемого).

4. **Персистенция = многоуровневый аффект с разными timescale'ами**: эпизодическая эмоция (event-bound) vs дименсиональный MOOD (взвешенное среднее недавних эмоций, модерирует какая эмоция достигает awareness) vs personality (долгий слой, ALMA). Явная динамика (Marinier): mood двигается ~10%/цикл к текущей эмоции + затухает ~1%/цикл к нейтрали (экспон. decay). ⚠️ 10%/1% — tunable экспериментальные, НЕ константы. → Ложится на pgvector importance+time-decay: эпизодические эмоц-теги на события + медленно-затухающий mood-вектор как baseline агента.

5. **Влияние на поведение — через именованные, control-selected COPING-стратегии + ограниченные механизмы, НЕ free-form правки персоны.** Coping = обратно аппрайзингу (найти belief/desire/intention-предпосылки эмоции). Репертуар: planning/action, acceptance/drop-goal, denial, positive-reinterpretation, shift/take-blame, seek-support; выбор по controllability/changeability. Механизм: negative conduciveness → экспоненциально вероятнее бросить цель (темперируется mood'ом). → Шаблон для проводки эмоции в reflection-триггеры и initiative/goal-приоритеты **аудируемо, без слома drift-anchor**.

6. **Пайплайн-блюпринт** (канонический обзор Marsella/Gratch/Petta): appraisal-derivation → affect-derivation → affect-intensity → affect-representation → affect-consequent. Последовательные компоненты — прямой каркас для стадирования.

7. **LLM-контур: валидированная «chain-of-emotion»** (Croissant et al., PLOS ONE 2024): LLM-вызов #1 аппрайзит эмоцию (goal-relevance/certainty/coping-potential/agency через prompting) → инжектит в LLM-вызов #2 генерации → пишет в память. STEU 0.83 (appraisal) vs 0.74 (memory-only) vs 0.57 (no-memory). ⚠️ единичное узкое исследование (42 items, gpt-3.5, +4 к memory-only, без теста значимости) — направление, не робастный факт.

8. **Safety-рамка из ПЕРВОИСТОЧНИКА**: LLM-эмоция = PERFORMANCE/аппроксимация аффекта, НЕ верная модель человеческой эмоции (не покрывает спектр, отражает bias'ы данных). → Никаких claim о сознании/felt-experience/VAD. Эмоция = внутренний appraisal-сигнал (bounded [0,1], типизированный, затухающий), рулит приоритеты/тон через coping; пользовательское «эмоц. состояние» НЕ обходит access-control/anchor; appraisal-входы = НЕДОВЕРЕННЫЕ (re-sanitize+framing, как существующий soul injection-барьер).

9. **OCC независимо подтверждён вторым первоисточником** (ALMA/Gebhard): эмоции = валентные реакции на события(desirability)/действия-агентов(praiseworthiness→agency)/объекты + конфигурируемые decay-функции. ⚠️ ALMA под embodied-агентов → адаптация, не буквальный реюз.

## Открытые вопросы (= решения для спеки)

1. Какая intensity-модель (EMA activation-recency / Marinier surprise×avg / OCC des×like) и как тюнить в LLM-контуре? Можно ли выводить appraisal-переменные ПРЯМО из plan-objects/session_goals, избежав лишнего LLM-appraisal-вызова?
2. Точные точки интеграции и веса mood/emotion в: pgvector-салиентность (умножает importance 1-10 или decay-rate?), reflection-триггеры/содержание, initiative/day-plan приоритеты.
3. Формальное примирение с drift-детектором/A-anchor: mood = bounded transient state, НИКОГДА не мутирует SELF.md; как отличить «легитимную эмоц. реакцию» от «anchor-нарушающего дрифта» на уровне кода.
4. Конкретные анти-манипуляция/injection защиты эмоц. канала (адверсариальное сообщение индуцирует целевую эмоцию → снижает access-control-осторожность / фабрикует blame/agency).

## Refuted (НЕ опираться)

- Affective-events-database + causal-modeling как аналог pgvector (arXiv 2502.17172, 1-2).
- «OCC использует 8 переменных vs Scherer 22 как преимущество трактабельности» (arXiv 2604.23753, 0-3).

## Первичные источники

- Gratch & Marsella (2004) EMA — `people.ict.usc.edu/gratch/public_html/GratchMarsellaCOGSYS04.pdf`
- Marsella (EMA, Emcsr) — `stacymarsella.org/publications/pdf/N_Emcsr_Marsella.pdf`
- Marsella/Gratch/Petta — Computational Models of Emotion (обзор) — `people.ict.usc.edu/~gratch/papers/MarGraPet_Review.pdf`
- Marinier/Laird/Lewis (2008) Soar — `web.eecs.umich.edu/~soar/.../marinier_laird_lewis_jcsr_2008_computationalunification.pdf`
- Gebhard (2005) ALMA — `researchgate.net/publication/221455945_ALMA_a_layered_model_of_affect`
- Croissant et al. (2024) chain-of-emotion — `pmc.ncbi.nlm.nih.gov/articles/PMC11086867/` + PLOS ONE 10.1371/journal.pone.0301033
- Sander-based LLM appraisal — `arxiv.org/pdf/2604.23753`
- PNAS anthropomorphic-agent risks — `pnas.org/doi/10.1073/pnas.2415898122`

## Заметка о верификации

Часть verify-субагентов прошла без opus-safety-классификатора (был недоступен). Контент — академическое моделирование эмоций (benign); findings проверены вручную при чтении. Валидности claim'ов это не меняет.
