export const changeStory = {
  "behavior": {
    "name": "passes_on_retriable",
    "checker": "replay_declined_charge",
    "cases": [
      true,
      false
    ],
    "expect": "expect trace.response.body.retriable == retriable"
  }
};
