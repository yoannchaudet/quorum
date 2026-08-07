<p align="center">
  <img src="icons/icon.svg" alt="Quorum logo" width="120" height="120" />
</p>

# Quorum

Quorum is my personal, lightweight harness for leveraging AI. Built on top of the `copilot` CLI and GitHub, it standardizes how I turn "work items" into plans that later get implemented and reviewed.

Nothing fancy, nothing magic — just some standardization and light automation. As AI models progress they get slower, and the development life cycle suffers. Quorum fills some of that gap by automating the menial tasks.

It was designed without token economy in mind and under little to no budget constraint. Things are changing and will keep changing; Quorum will either evolve or cease to exist. Still, it is an attempt at maximizing my use of AI to create value with the limited time I have.

Where does the name come from? I believe all models will eventually converge, and that any of them can do a pretty good job of following a plan. Planning, to me, is one of the most important steps — like writing the specification for a piece of work: with a good foundation, the work goes well. So the planning phase is delegated to multiple models (hence "Quorum"), and the overall plan is pieced together from all their ideas. A single implementer then works against an adversarial reviewer (a different model). Each phase is a loop. Humans stay involved:

- At intake — providing a work item as a GitHub issue or a markdown prompt, and answering the planners' follow-up questions
- Optionally, to review the finalized plan
- Optionally, to review the finalized work

## Documentation

The Core and CLI are specified under [`docs/`](docs/README.md).
