FROM python:3.12-slim
WORKDIR /app
COPY deploy/search/requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r requirements.txt
COPY search_app.py search_snapshot.py profile_store.py /app/
COPY static /app/static
USER 65534:65534
EXPOSE 8001
CMD ["python", "search_app.py", "--host", "0.0.0.0", "--snapshot"]
